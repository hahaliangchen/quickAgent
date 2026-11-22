#[derive(Default)]
struct ElementRefs {
    document: Value,
    nodes: std::collections::HashMap<String, i64>,
    next: u64,
}

impl ElementRefs {
    fn observe(&mut self, document: Value) {
        if self.document != document {
            self.nodes.clear();
            self.document = document;
        }
    }
}

async fn ref_document(cdp: &mut CdpConnection) -> Result<Value> {
    let reply = cdp.call("Page.getFrameTree", json!({})).await?;
    let frame = reply
        .pointer("/result/frameTree/frame")
        .context("no current browser document")?;
    Ok(json!([
        frame["id"],
        frame["loaderId"],
        frame["url"],
        frame["urlFragment"]
    ]))
}

fn interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "checkbox"
            | "radio"
            | "combobox"
            | "listbox"
            | "option"
            | "tab"
            | "switch"
            | "slider"
            | "spinbutton"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "treeitem"
            | "DisclosureTriangle"
            | "ColorWell"
            | "togglebutton"
            | "scrollbar"
    )
}

async fn read_elements(session: &mut Session, limit: usize) -> Result<Vec<Value>> {
    let mut cdp = connect_cdp(&session.ws).await?;
    let before = ref_document(&mut cdp).await?;
    let tree = cdp.call("Accessibility.getFullAXTree", json!({})).await?;
    let snapshot = cdp
        .call(
            "DOMSnapshot.captureSnapshot",
            json!({"computedStyles":["visibility","opacity"]}),
        )
        .await?;
    let after = ref_document(&mut cdp).await?;
    session.refs.observe(after.clone());
    ensure!(
        before == after,
        "page changed while reading elements; call browser.read() again"
    );
    let doc = &snapshot["result"]["documents"][0];
    let strings = snapshot["result"]["strings"]
        .as_array()
        .context("missing DOM snapshot strings")?;
    let indices = doc["layout"]["nodeIndex"]
        .as_array()
        .context("missing DOM layout")?;
    let backend = doc["nodes"]["backendNodeId"]
        .as_array()
        .context("missing DOM backend IDs")?;
    let parent = doc["nodes"]["parentIndex"]
        .as_array()
        .context("missing DOM parent indices")?;
    let style = &doc["layout"]["styles"];
    let bounds = &doc["layout"]["bounds"];
    let css = |idx: &Value| {
        idx.as_u64()
            .and_then(|i| strings.get(i as usize))
            .and_then(Value::as_str)
    };
    let transparent: std::collections::HashSet<_> = indices
        .iter()
        .enumerate()
        .filter_map(|(i, node)| {
            (css(&style[i][1]).and_then(|v| v.parse::<f64>().ok()) == Some(0.0))
                .then(|| node.as_u64())
                .flatten()
        })
        .collect();
    let mut visible = std::collections::HashSet::new();
    for (i, index) in indices.iter().enumerate() {
        let Some(index) = index.as_u64() else {
            continue;
        };
        if css(&style[i][0]) != Some("visible")
            || !(2..=3).all(|n| bounds[i][n].as_f64().is_some_and(|v| v > 0.0))
        {
            continue;
        }
        let mut ancestor = Some(index);
        while let Some(node) = ancestor.filter(|node| !transparent.contains(node)) {
            ancestor = parent
                .get(node as usize)
                .and_then(Value::as_u64)
                .filter(|p| *p < node);
        }
        if ancestor.is_none()
            && let Some(id) = backend.get(index as usize).and_then(Value::as_i64)
        {
            visible.insert(id);
        }
    }
    let previous: std::collections::HashMap<_, _> = session
        .refs
        .nodes
        .iter()
        .map(|(key, id)| (*id, key.clone()))
        .collect();
    let mut next_nodes = std::collections::HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut elements = Vec::new();
    for node in tree["result"]["nodes"]
        .as_array()
        .context("missing accessibility tree")?
    {
        if elements.len() == limit {
            break;
        }
        let role = node["role"]["value"].as_str().unwrap_or("");
        let Some(id) = node["backendDOMNodeId"].as_i64() else {
            continue;
        };
        if node["ignored"] == true
            || !interactive_role(role)
            || !visible.contains(&id)
            || !seen.insert(id)
        {
            continue;
        }
        let reference = previous.get(&id).cloned().unwrap_or_else(|| {
            session.refs.next += 1;
            format!("e{}", session.refs.next)
        });
        let name = node["name"]["value"].as_str().unwrap_or("");
        let mut element =
            json!({"ref":reference,"role":role,"name":&name[..name.floor_char_boundary(160)]});
        // AX returns a masked password value. Do not publish it even in masked form.
        let value = &node["value"]["value"];
        if let Some(value) = value.as_str() {
            if !value.starts_with(['\u{2022}', '\u{25cf}']) {
                element["value"] = json!(&value[..value.floor_char_boundary(160)]);
            }
        } else if value.is_number() || value.is_boolean() {
            element["value"] = value.clone();
        }
        if let Some(properties) = node["properties"].as_array() {
            for property in properties {
                if let Some(key @ ("checked" | "selected" | "expanded" | "disabled")) =
                    property["name"].as_str()
                {
                    element[key] = property["value"]["value"].clone();
                }
            }
        }
        next_nodes.insert(reference, id);
        elements.push(element);
    }
    // A smaller read budget must not invalidate refs already issued for this document.
    // Keep the cache bounded and drop IDs for nodes no longer in its DOM.
    let attached: std::collections::HashSet<_> = backend.iter().filter_map(Value::as_i64).collect();
    let mut older: Vec<_> = session
        .refs
        .nodes
        .drain()
        .filter(|(key, id)| !next_nodes.contains_key(key) && attached.contains(id))
        .collect();
    older.sort_unstable_by_key(|(key, _)| std::cmp::Reverse(key[1..].parse::<u64>().unwrap_or(0)));
    let budget = 4096usize.saturating_sub(next_nodes.len());
    next_nodes.extend(older.into_iter().take(budget));
    session.refs.nodes = next_nodes;
    Ok(elements)
}

#[derive(Debug, thiserror::Error)]
#[error("element ref {0} is stale; call browser.read() to refresh")]
struct StaleRef(String);

async fn resolve_element(
    cdp: &mut CdpConnection,
    refs: &ElementRefs,
    reference: &str,
) -> Result<String> {
    let stale = || StaleRef(reference.to_owned());
    let id = refs.nodes.get(reference).ok_or_else(stale)?;
    ensure!(ref_document(cdp).await? == refs.document, stale());
    let resolved = cdp
        .call("DOM.resolveNode", json!({"backendNodeId":id}))
        .await;
    let resolved = match resolved {
        Err(error)
            if is_stale_node_error(&error)
                || error.to_string().contains("No node with given id") =>
        {
            return Err(stale().into());
        }
        result => result?,
    };
    let object = resolved
        .pointer("/result/object/objectId")
        .and_then(Value::as_str)
        .ok_or_else(stale)?;
    let connected = cdp.call("Runtime.callFunctionOn", json!({"objectId":object,"returnByValue":true,
        "functionDeclaration":"function(){return this.isConnected && this.ownerDocument===document}"})).await;
    match connected {
        Err(error) if is_stale_node_error(&error) => return Err(stale().into()),
        result => ensure!(
            result?.pointer("/result/result/value") == Some(&json!(true)),
            stale()
        ),
    }
    Ok(object.to_owned())
}

async fn element_function(
    cdp: &mut CdpConnection,
    object: &str,
    reference: Option<&str>,
    function: &str,
    arguments: Value,
) -> Result<Value> {
    let reply = cdp
        .call(
            "Runtime.callFunctionOn",
            json!({"objectId":object,"returnByValue":true,
        "functionDeclaration":function,"arguments":arguments}),
        )
        .await;
    let reply = match reply {
        Err(error) if reference.is_some() && is_stale_node_error(&error) => {
            return Err(StaleRef(reference.unwrap().to_owned()).into());
        }
        result => result?,
    };
    let value = reply
        .pointer("/result/result/value")
        .context("cannot operate on this browser element")?;
    if let Some(reference) = reference {
        ensure!(value["stale"] != true, StaleRef(reference.to_owned()));
    }
    Ok(value.clone())
}

const POINTER_POSITION: &str = r#"function(selector, click) {
    const el=selector===null ? this : this.querySelector(selector);
    if (selector===null && !el.isConnected) return {stale:true};
    if (!el || !el.getClientRects().length || !el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) ||
        (click && (el.disabled || el.getAttribute('aria-disabled')==='true'))) return {ok:false};
    el.scrollIntoView({block:'center',inline:'center',behavior:'instant'});
    const r=el.getBoundingClientRect();
    const l=Math.max(0,r.left), t=Math.max(0,r.top), b=Math.min(innerHeight,r.bottom), right=Math.min(innerWidth,r.right);
    const x=(l+right)/2, y=(t+b)/2;
    const hit=el.getRootNode().elementFromPoint(x,y);
    return {ok:right>l&&b>t&&el.contains(hit),x,y,href:click ? el.closest('a[href]')?.href : null,label:(el.getAttribute('aria-label')||el.innerText||el.tagName).slice(0,160)};
}"#;

async fn document_object(cdp: &mut CdpConnection) -> Result<String> {
    let reply = cdp
        .call("Runtime.evaluate", json!({"expression":"document"}))
        .await?;
    reply
        .pointer("/result/result/objectId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("page document is unavailable")
}

async fn pointer_action(session: &Session, args: &Value, click: bool) -> Result<Value> {
    let reference = args["ref"].as_str();
    let selector = args["selector"].as_str();
    ensure!(
        reference.is_some() || selector.is_some_and(|s| !s.trim().is_empty()),
        "a ref or selector is required"
    );
    let mut cdp = connect_cdp(&session.ws).await?;
    let times = if click {
        args["times"]
            .as_u64()
            .unwrap_or(1)
            .clamp(1, MAX_REPEATED_CLICKS)
    } else {
        1
    };
    let mut label = Value::Null;
    for index in 0..times {
        if index > 0 {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        let object = if let Some(reference) = reference {
            resolve_element(&mut cdp, &session.refs, reference).await?
        } else {
            document_object(&mut cdp).await?
        };
        let point = element_function(
            &mut cdp,
            &object,
            reference,
            POINTER_POSITION,
            json!([{"value":selector},{"value":click}]),
        )
        .await?;
        ensure!(
            point["ok"] == true,
            "element is hidden, disabled, or covered"
        );
        if click {
            check_link_destination(&point["href"])?;
            for kind in ["mousePressed", "mouseReleased"] {
                cdp.call("Input.dispatchMouseEvent", json!({"type":kind,"x":point["x"],"y":point["y"],"button":"left","clickCount":1})).await?;
            }
        } else {
            cdp.call(
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":point["x"],"y":point["y"]}),
            )
            .await?;
        }
        label = point["label"].clone();
    }
    Ok(if click {
        json!({"clicked":label,"matched":true,"match":"ref","ref":reference,"candidates":1,"times":times})
    } else {
        json!({"hovered":label,"ref":reference,"selector":selector})
    })
}
