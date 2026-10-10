use russh::{client, ChannelMsg};
use russh::keys::PublicKeyOrCertificate;
use std::sync::Arc;
use crossbeam_channel::Sender;

pub struct SshHandler {}

impl client::Handler for SshHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true) // Accept all keys for now
    }
}

pub async fn start_ssh_terminal(
    host: String,
    port: u16,
    user: String,
    pass: String,
    ui_tx: Sender<crate::messages::SshMessage>,
    mut input_rx: tokio::sync::mpsc::Receiver<String>,
) {
    let config = Arc::new(client::Config::default());
    let sh = SshHandler {};
    
    let mut session = match client::connect(config, (host, port), sh).await {
        Ok(s) => s,
        Err(e) => {
            let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Connection failed: {}", e)));
            return;
        }
    };
    
    let auth_res = match session.authenticate_password(user, pass).await {
        Ok(r) => r,
        Err(e) => {
            let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Auth error: {}", e)));
            return;
        }
    };
    
    if !matches!(auth_res, client::AuthResult::Success) {
        let _ = ui_tx.send(crate::messages::SshMessage::Error("Authentication failed".into()));
        return;
    }
    
    let mut channel = match session.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Channel error: {}", e)));
            return;
        }
    };
    
    // Request a PTY
    if let Err(e) = channel.request_pty(true, "xterm", 80, 24, 0, 0, &[]).await {
        let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("PTY error: {}", e)));
    }
    
    // Request shell
    if let Err(e) = channel.request_shell(true).await {
        let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Shell error: {}", e)));
    }
    
    // Send connected msg
    let _ = ui_tx.send(crate::messages::SshMessage::Output("Connected to SSH shell.\n".into()));

    loop {
        tokio::select! {
            input_opt = input_rx.recv() => {
                if let Some(mut input) = input_opt {
                    if !input.ends_with('\n') {
                        input.push('\n');
                    }
                    let _ = channel.data_bytes(input.into_bytes()).await;
                } else {
                    // channel closed
                    break;
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        let text = String::from_utf8_lossy(&data).to_string();
                        let _ = ui_tx.send(crate::messages::SshMessage::Output(text));
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let text = String::from_utf8_lossy(&data).to_string();
                        let _ = ui_tx.send(crate::messages::SshMessage::Output(text));
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        let _ = ui_tx.send(crate::messages::SshMessage::Disconnected);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}
