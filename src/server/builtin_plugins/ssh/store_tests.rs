use super::*;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    sync::Barrier,
    thread,
};

fn draft(name: &str, auth_type: AuthType, env_tier: EnvTier) -> NewServer {
    NewServer {
        name: name.into(),
        host: " ssh.example.invalid ".into(),
        port: None,
        username: " deploy ".into(),
        auth_type,
        key_path: (auth_type == AuthType::KeyFile).then(|| "/local/private-key".into()),
        env_tier,
        require_approval: false,
        default_dir: Some("/srv/project".into()),
        tags: vec!["team".into()],
        notes: None,
    }
}

#[test]
fn initializes_per_database_and_enforces_profile_settings() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("config");
    let store = Store::open(&root)?;
    let prod = store.create(draft("Prod", AuthType::KeyFile, EnvTier::Prod))?;
    let dev = store.create(draft("dev", AuthType::Agent, EnvTier::Dev))?;
    let mut staging = draft("staging", AuthType::Agent, EnvTier::Staging);
    staging.port = Some(2222);
    staging.require_approval = true;
    let stage = store.create(staging)?;
    let mut invalid = draft("bad-key", AuthType::KeyFile, EnvTier::Dev);
    invalid.key_path = Some("relative.key".into());
    assert!(store.create(invalid).is_err());
    assert!(
        store
            .db
            .execute(
                "UPDATE ssh_servers SET require_approval = 0 WHERE id = ?1",
                [&prod]
            )
            .is_err()
    );

    let reopened = Store::open(&root)?;
    let list = reopened.list()?;
    assert_eq!(
        list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        [&dev, &prod, &stage]
    );
    assert_eq!(list[0].host, "ssh.example.invalid");
    assert_eq!(list[0].username, "deploy");
    assert_eq!(list[0].port, 22);
    assert!(!list[0].require_approval);
    assert_eq!(list[1].key_path.as_deref(), Some("/local/private-key"));
    assert!(list[1].require_approval);
    assert!(list[2].require_approval);
    assert_eq!(list[2].port, 2222);
    assert!(
        Store::open(&dir.path().join("separate"))?
            .list()?
            .is_empty()
    );
    store.db.pragma_update(None, "user_version", 2)?;
    let Err(error) = Store::open(&root) else {
        panic!("accepted newer schema")
    };
    assert!(
        error
            .to_string()
            .contains("unsupported SSH workbench database version")
    );
    Ok(())
}

#[test]
fn credentials_stay_out_of_sqlite_and_public_profiles() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(dir.path())?;
    let pwd = store.create(draft("password", AuthType::Password, EnvTier::Dev))?;
    let key = store.create(draft("key", AuthType::KeyFile, EnvTier::Dev))?;
    let agent = store.create(draft("agent", AuthType::Agent, EnvTier::Dev))?;
    assert!(store.auth_secret(&pwd)?.is_none());
    assert!(!store.credentials_path.exists());
    assert!(
        store
            .set_credential(&agent, Some(Secret::new("agent-denied".into())?))
            .is_err()
    );
    store.set_credential(&pwd, Some(Secret::new("unique-password-value".into())?))?;
    store.set_credential(&key, Some(Secret::new("unique-passphrase-value".into())?))?;
    let mode = fs::metadata(&store.credentials_path)?.permissions().mode();
    assert_eq!(mode & 0o7777, 0o600);
    let reopened = Store::open(dir.path())?;
    assert_eq!(
        reopened.auth_secret(&pwd)?.unwrap().expose(),
        "unique-password-value"
    );
    assert_eq!(
        reopened.auth_secret(&key)?.unwrap().expose(),
        "unique-passphrase-value"
    );
    let public = serde_json::to_value(reopened.list()?)?;
    assert_eq!(public[1]["has_passphrase"], true);
    assert_eq!(public[1]["has_password"], false);
    assert_eq!(public[2]["has_password"], true);
    assert_eq!(public[2]["has_passphrase"], false);
    assert!(public[1].get("key_path").is_none());
    let db_bytes = fs::read(dir.path().join("ssh-workbench.sqlite3"))?;
    let public_repr = public.to_string();
    for value in ["unique-password-value", "unique-passphrase-value"] {
        assert!(
            !db_bytes
                .windows(value.len())
                .any(|window| window == value.as_bytes())
        );
        assert!(!public_repr.contains(value));
    }
    assert!(store.delete(&key)?);
    assert!(store.get(&key)?.is_none());
    assert!(store.auth_secret(&key).is_err());
    assert!(!fs::read_to_string(&store.credentials_path)?.contains("unique-passphrase-value"));
    assert!(reopened.auth_secret(&pwd)?.is_some());
    store.set_credential(&pwd, None)?;
    assert!(reopened.auth_secret(&pwd)?.is_none());
    assert!(!reopened.get(&pwd)?.unwrap().has_password);
    Ok(())
}

#[test]
fn rejects_unsafe_or_corrupt_credential_files_without_leaking_contents() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(dir.path())?;
    let id = store.create(draft("key", AuthType::Password, EnvTier::Dev))?;
    let path = store.credentials_path.clone();
    store.set_credential(&id, Some(Secret::new("stored-private-value".into())?))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    let Err(error) = store.auth_secret(&id) else {
        panic!("accepted public permissions")
    };
    assert!(error.to_string().contains("mode 0600"));
    assert!(store.delete(&id).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let renamed = dir.path().join("renamed.json");
    fs::rename(&path, &renamed)?;
    symlink(&renamed, &path)?;
    assert!(store.auth_secret(&id).is_err());
    assert!(
        store
            .set_credential(&id, Some(Secret::new("must-not-replace".into())?))
            .is_err()
    );
    assert!(fs::read_to_string(&renamed)?.contains("stored-private-value"));
    fs::remove_file(&path)?;
    fs::rename(&renamed, &path)?;
    fs::write(
        &path,
        br#"{"bad":{"kind":"do-not-leak-this","value":"another-secret"}}"#,
    )?;
    let Err(error) = store.auth_secret(&id) else {
        panic!("accepted invalid JSON credentials")
    };
    assert_eq!(format!("{error:#}"), "invalid SSH credential file");
    assert!(Store::open(dir.path())?.get(&id).is_err());
    Ok(())
}

#[test]
fn tofu_pins_rejects_changed_key_and_requires_exact_confirmation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut first = Store::open(dir.path())?;
    let id = first.create(draft("agent", AuthType::Agent, EnvTier::Dev))?;
    let trusted = HostKeyFingerprint::of_key_blob(b"abc");
    let changed = HostKeyFingerprint::of_key_blob(b"changed-key");
    let third = HostKeyFingerprint::of_key_blob(b"third-key");
    assert_eq!(
        trusted.to_string(),
        "SHA256:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0"
    );
    assert!(first.mark_connected(&id, trusted).is_err());
    assert!(first.confirm_host_key(&id, changed, trusted).is_err());
    first.observe_host_key(&id, trusted)?;
    assert!(first.get(&id)?.unwrap().last_connected_at.is_none());
    first.mark_connected(&id, trusted)?;
    let mut reopened = Store::open(dir.path())?;
    reopened.observe_host_key(&id, trusted)?;
    let err = reopened.observe_host_key(&id, changed).unwrap_err();
    let mismatch = err.downcast_ref::<HostKeyMismatch>().unwrap();
    assert_eq!((mismatch.trusted, mismatch.received), (trusted, changed));
    assert!(err.to_string().starts_with("HOST_KEY_MISMATCH:"));
    assert_eq!(first.get(&id)?.unwrap().host_key_fingerprint, Some(trusted));
    assert!(first.get(&id)?.unwrap().last_connected_at.is_some());
    reopened.confirm_host_key(&id, trusted, changed)?;
    assert!(first.confirm_host_key(&id, trusted, third).is_err());
    assert!(first.mark_connected(&id, trusted).is_err());
    assert!(first.observe_host_key(&id, trusted).is_err());
    first.observe_host_key(&id, changed)?;
    first.mark_connected(&id, changed)?;
    Ok(())
}

#[test]
fn concurrent_connections_preserve_both_credentials_and_only_one_first_key() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    let store = Store::open(root)?;
    let ids = [
        store.create(draft("first", AuthType::Password, EnvTier::Dev))?,
        store.create(draft("second", AuthType::Password, EnvTier::Dev))?,
    ];
    let ready = Barrier::new(2);
    let outcomes = thread::scope(|scope| {
        let jobs = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let (target, ready) = (&ids[0], &ready);
                scope.spawn(move || -> Result<_> {
                    ready.wait();
                    let mut store = Store::open(root)?;
                    store.set_credential(id, Some(Secret::new(format!("credential-{i}"))?))?;
                    let key = HostKeyFingerprint::of_key_blob(&[i as u8]);
                    Ok((key, store.observe_host_key(target, key)))
                })
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Result<Vec<_>>>()
    })?;
    let stored = store.get(&ids[0])?.unwrap().host_key_fingerprint.unwrap();
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    for (i, (key, result)) in outcomes.into_iter().enumerate() {
        assert_eq!(
            store.auth_secret(&ids[i])?.unwrap().expose(),
            format!("credential-{i}")
        );
        if key == stored {
            result?;
        } else {
            assert!(result.unwrap_err().is::<HostKeyMismatch>());
        }
    }
    Ok(())
}

#[test]
fn usage_is_scoped_sorted_capped_and_cascades_on_delete() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(dir.path())?;
    let id = store.create(draft("primary", AuthType::Agent, EnvTier::Dev))?;
    let other = store.create(draft("other", AuthType::Agent, EnvTier::Dev))?;
    for action in [
        Action::Terminal,
        Action::ReadFile,
        Action::Upload,
        Action::Download,
        Action::WriteFile,
    ] {
        store.record_usage(
            &id,
            action,
            Some("task-1"),
            Some("/local/project"),
            Some("/srv/project"),
        )?;
    }
    store.record_usage(&other, Action::Terminal, None, None, None)?;
    let history = store.history(&id, 1)?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].task_id.as_deref(), Some("task-1"));
    assert_eq!(history[0].workspace_root.as_deref(), Some("/local/project"));
    assert_eq!(history[0].remote_path.as_deref(), Some("/srv/project"));
    assert_eq!(history[0].action, "write_file");
    for _ in 0..200 {
        store.record_usage(&id, Action::ReadFile, None, None, None)?;
    }
    assert_eq!(store.history(&id, u32::MAX)?.len(), 200);
    assert!(store.delete(&id)?);
    assert!(!store.delete(&id)?);
    assert!(store.history(&id, 10)?.is_empty());
    assert!(
        store
            .record_usage(&id, Action::Terminal, None, None, None)
            .is_err()
    );
    assert_eq!(store.history(&other, 10)?.len(), 1);
    Ok(())
}
