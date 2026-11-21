async fn eval(ws: &str, expression: &str) -> Result<Value> {
    let mut cdp = connect_cdp(ws).await?;
    evaluate(&mut cdp, expression).await
}

async fn evaluate(cdp: &mut CdpConnection, expression: &str) -> Result<Value> {
    let response = cdp
        .call(
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true}),
        )
        .await?;
    ensure!(
        response.pointer("/result/exceptionDetails").is_none(),
        "page script failed: {}",
        response
            .pointer("/result/exceptionDetails/text")
            .and_then(Value::as_str)
            .unwrap_or("exception")
    );
    response
        .pointer("/result/result/value")
        .cloned()
        .context("page script returned no value")
}

async fn call(ws: &str, method: &str, params: Value) -> Result<Value> {
    let mut connection = connect_cdp(ws).await?;
    connection.call(method, params).await
}

async fn connect_cdp(ws: &str) -> Result<CdpConnection> {
    Ok(CdpConnection {
        stream: connect(ws).await?,
        next_id: 1,
        dialog: None,
    })
}

async fn watch_dialogs(
    ws: &str,
    pending: Option<Value>,
) -> Result<(
    std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    tokio::sync::mpsc::Sender<DialogCommand>,
    tokio::task::JoinHandle<()>,
)> {
    let mut cdp = connect_cdp(ws).await?;
    if pending.is_some() {
        // Chrome may defer Page.enable until the existing modal is handled.
        send(
            &mut cdp.stream,
            &json!({"id":cdp.next_id,"method":"Page.enable","params":{}}).to_string(),
        )
        .await?;
        cdp.next_id += 1;
    } else {
        cdp.call("Page.enable", json!({})).await?;
    }
    let dialog = std::sync::Arc::new(std::sync::Mutex::new(cdp.dialog.take().or(pending)));
    let observed = dialog.clone();
    let (commands, mut incoming) = tokio::sync::mpsc::channel::<DialogCommand>(1);
    let watch = tokio::spawn(async move {
        // Keep Page events attached between model requests; a new short-lived
        // Runtime connection cannot reliably rediscover an already-open dialog.
        let (mut reader, mut writer) = cdp.stream.into_split();
        let mut replies = std::collections::HashMap::new();
        loop {
            // Keep the frame read alive while writing commands: read_exact is
            // not cancellation safe if a WebSocket frame is only partly read.
            let message = recv(&mut reader);
            tokio::pin!(message);
            loop {
                tokio::select! {
                    message=&mut message=> {
                        let Ok(message)=message else {return;};
                        if let Ok(event)=serde_json::from_str::<Value>(&message) {
                            if event["method"]=="Page.javascriptDialogOpening" {*observed.lock().unwrap()=Some(event["params"].clone());}
                            if event["method"]=="Page.javascriptDialogClosed" {*observed.lock().unwrap()=None;}
                            if let Some(reply)=event["id"].as_u64().and_then(|id|replies.remove(&id)) {
                                if event.get("error").is_none() {*observed.lock().unwrap()=None;}
                                let _=tokio::sync::oneshot::Sender::send(reply,Ok(event));
                            }
                        }
                        break;
                    }
                    command=incoming.recv()=> {
                        let Some((accept,reply))=command else {return;};
                        let id=cdp.next_id;cdp.next_id+=1;
                        match send(&mut writer,&json!({"id":id,"method":"Page.handleJavaScriptDialog","params":{"accept":accept}}).to_string()).await {
                            Ok(())=>{replies.insert(id,reply);}
                            Err(error)=>{let _=reply.send(Err(error));return;}
                        }
                    }
                }
            }
        }
    });
    Ok((dialog, commands, watch))
}

impl CdpConnection {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let allowed_with_dialog = matches!(
            method,
            "Page.enable" | "Page.handleJavaScriptDialog" | "Browser.close"
        );
        if !allowed_with_dialog && self.dialog.is_some() {
            anyhow::bail!("browser_dialog_open: {}", self.dialog.as_ref().unwrap());
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("browser command id space exhausted")?;
        send(
            &mut self.stream,
            &json!({"id":id,"method":method,"params":params}).to_string(),
        )
        .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let message = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                recv(&mut self.stream),
            )
            .await
            .context("browser command timed out")??;
            let value: Value = serde_json::from_str(&message)?;
            if value["method"] == "Page.javascriptDialogOpening" {
                self.dialog = Some(value["params"].clone());
                if !allowed_with_dialog {
                    anyhow::bail!("browser_dialog_open: {}", value["params"]);
                }
            }
            if value["method"] == "Page.javascriptDialogClosed" {
                self.dialog = None;
            }
            if value["id"].as_u64() == Some(id) {
                if let Some(error) = value.get("error") {
                    anyhow::bail!("{method} failed: {error}");
                }
                return Ok(value);
            }
        }
    }
}

async fn connect(ws_url: &str) -> Result<TcpStream> {
    let url = reqwest::Url::parse(ws_url)?;
    let host = url.host_str().context("browser socket has no host")?;
    let port = url
        .port_or_known_default()
        .context("browser socket has no port")?;
    let mut stream = TcpStream::connect((host, port)).await?;
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        url.path()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut header = Vec::new();
    while !header.windows(4).any(|mark| mark == b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        header.push(byte[0]);
        ensure!(header.len() < 8192, "browser upgrade header is too large");
    }
    let text = String::from_utf8_lossy(&header);
    ensure!(
        text.contains("101"),
        "browser did not accept the websocket: {text}"
    );
    Ok(stream)
}

async fn send(stream: &mut (impl tokio::io::AsyncWrite + Unpin), text: &str) -> Result<()> {
    let payload = text.as_bytes();
    ensure!(payload.len() < 1_000_000, "browser command is too large");
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend((payload.len() as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend((payload.len() as u64).to_be_bytes());
    }
    let mask = [1u8, 2, 3, 4];
    frame.extend(mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    stream.write_all(&frame).await?;
    Ok(())
}

async fn recv(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<String> {
    let mut message = Vec::new();
    loop {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).await?;
        let opcode = header[0] & 0x0f;
        let mut length = (header[1] & 0x7f) as usize;
        if length == 126 {
            let mut extended = [0u8; 2];
            stream.read_exact(&mut extended).await?;
            length = u16::from_be_bytes(extended) as usize;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            stream.read_exact(&mut extended).await?;
            length = usize::try_from(u64::from_be_bytes(extended))?;
        }
        ensure!(
            length <= 64 * 1024 * 1024 && message.len() + length <= 64 * 1024 * 1024,
            "browser message exceeds capture budget"
        );
        let masked = header[1] & 0x80 != 0;
        let mut mask = [0u8; 4];
        if masked {
            stream.read_exact(&mut mask).await?;
        }
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).await?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        ensure!(
            opcode == 1 || opcode == 0,
            "browser returned a non-text/closed frame"
        );
        message.extend(payload);
        if header[0] & 0x80 != 0 {
            return Ok(String::from_utf8(message)?);
        }
    }
}

