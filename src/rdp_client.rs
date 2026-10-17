use std::sync::mpsc::Sender;
use eframe::egui;
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{BitmapConfig, ClientConnector, Config, Credentials, DesktopSize, ServerName};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{Database as InputDatabase, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::{BitmapCodecs, MajorPlatformType};
use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageOutput};
use ironrdp_tokio::{connect_begin, connect_finalize, mark_as_upgraded, single_sequence_step, FramedWrite, TokioFramed};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const RDP_VAULT_FILE: &str = "rdp_vault.json";

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RdpProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub domain: String,
    pub user: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub enable_credssp: bool,
    #[serde(default = "default_true")]
    pub fast_mode: bool,
}

impl Default for RdpProfile {
    fn default() -> Self {
        Self {
            name: "Windows Server".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3389,
            domain: String::new(),
            user: "Administrator".to_string(),
            password: String::new(),
            enable_credssp: true,
            fast_mode: true,
        }
    }
}

/// Ensures RDP resolution complies with MS-RDPEDISP / codec alignment rules (even width divisible by 4, >= 640x480).
pub fn normalize_rdp_resolution(w: u16, h: u16) -> (u16, u16) {
    let cw = (w.clamp(640, 3840)) & !3;
    let ch = (h.clamp(480, 2160)) & !1;
    (cw.max(640), ch.max(480))
}

pub fn save_rdp_vault(master_password: &str, profiles: &[RdpProfile]) -> Result<(), String> {
    crate::ssh_client::save_encrypted_json(RDP_VAULT_FILE, master_password, &profiles)
}

pub fn unlock_rdp_vault(master_password: &str) -> Result<Vec<RdpProfile>, String> {
    if !std::path::Path::new(RDP_VAULT_FILE).exists() {
        return Ok(Vec::new());
    }
    crate::ssh_client::load_encrypted_json(RDP_VAULT_FILE, master_password)
}

pub enum RdpUiMessage {
    Connected {
        width: u16,
        height: u16,
    },
    FullFrame {
        width: u16,
        height: u16,
        rgba: Vec<u8>,
    },
    PartialFrame {
        total_width: u16,
        total_height: u16,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        rgba: Vec<u8>,
    },
    Error(String),
    Disconnected(String),
}

pub enum RdpInputMessage {
    Operations(Vec<Operation>),
    Resize { width: u16, height: u16 },
    Disconnect,
}

pub fn egui_key_to_scancode(key: egui::Key) -> Option<Scancode> {
    use egui::Key::*;
    let (ext, code): (bool, u8) = match key {
        Escape => (false, 0x01),
        Num1 => (false, 0x02),
        Num2 => (false, 0x03),
        Num3 => (false, 0x04),
        Num4 => (false, 0x05),
        Num5 => (false, 0x06),
        Num6 => (false, 0x07),
        Num7 => (false, 0x08),
        Num8 => (false, 0x09),
        Num9 => (false, 0x0A),
        Num0 => (false, 0x0B),
        Minus => (false, 0x0C),
        Plus | Equals => (false, 0x0D),
        Backspace => (false, 0x0E),
        Tab => (false, 0x0F),
        Q => (false, 0x10),
        W => (false, 0x11),
        E => (false, 0x12),
        R => (false, 0x13),
        T => (false, 0x14),
        Y => (false, 0x15),
        U => (false, 0x16),
        I => (false, 0x17),
        O => (false, 0x18),
        P => (false, 0x19),
        OpenBracket => (false, 0x1A),
        CloseBracket => (false, 0x1B),
        Enter => (false, 0x1C),
        A => (false, 0x1E),
        S => (false, 0x1F),
        D => (false, 0x20),
        F => (false, 0x21),
        G => (false, 0x22),
        H => (false, 0x23),
        J => (false, 0x24),
        K => (false, 0x25),
        L => (false, 0x26),
        Semicolon => (false, 0x27),
        Backtick => (false, 0x29),
        Backslash => (false, 0x2B),
        Z => (false, 0x2C),
        X => (false, 0x2D),
        C => (false, 0x2E),
        V => (false, 0x2F),
        B => (false, 0x30),
        N => (false, 0x31),
        M => (false, 0x32),
        Comma => (false, 0x33),
        Period => (false, 0x34),
        Slash => (false, 0x35),
        Space => (false, 0x39),
        F1 => (false, 0x3B),
        F2 => (false, 0x3C),
        F3 => (false, 0x3D),
        F4 => (false, 0x3E),
        F5 => (false, 0x3F),
        F6 => (false, 0x40),
        F7 => (false, 0x41),
        F8 => (false, 0x42),
        F9 => (false, 0x43),
        F10 => (false, 0x44),
        F11 => (false, 0x57),
        F12 => (false, 0x58),
        ArrowUp => (true, 0x48),
        ArrowLeft => (true, 0x4B),
        ArrowRight => (true, 0x4D),
        ArrowDown => (true, 0x50),
        Home => (true, 0x47),
        End => (true, 0x4F),
        PageUp => (true, 0x49),
        PageDown => (true, 0x51),
        Insert => (true, 0x52),
        Delete => (true, 0x53),
        _ => return None,
    };
    Some(Scancode::from_u8(ext, code))
}

pub fn collect_egui_rdp_operations(
    ui: &egui::Ui,
    response: &egui::Response,
    remote_w: u16,
    remote_h: u16,
) -> Vec<Operation> {
    let mut ops = Vec::new();
    let rect = response.rect;

    if let Some(pos) = response.hover_pos().or_else(|| ui.input(|i| i.pointer.interact_pos())) {
        if rect.contains(pos) && rect.width() > 1.0 && rect.height() > 1.0 {
            let rel_x = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
            let rel_y = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
            let rx = (rel_x * f32::from(remote_w.saturating_sub(1))) as u16;
            let ry = (rel_y * f32::from(remote_h.saturating_sub(1))) as u16;
            ops.push(Operation::MouseMove(MousePosition { x: rx, y: ry }));
        }
    }

    if response.hovered() || response.has_focus() {
        ui.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::PointerButton {
                        button,
                        pressed,
                        ..
                    } if response.hovered() => {
                        let mb = match button {
                            egui::PointerButton::Primary => Some(MouseButton::Left),
                            egui::PointerButton::Secondary => Some(MouseButton::Right),
                            egui::PointerButton::Middle => Some(MouseButton::Middle),
                            _ => None,
                        };
                        if let Some(b) = mb {
                            if *pressed {
                                ops.push(Operation::MouseButtonPressed(b));
                            } else {
                                ops.push(Operation::MouseButtonReleased(b));
                            }
                        }
                    }
                    egui::Event::MouseWheel { delta, .. } if response.hovered() => {
                        if delta.y.abs() > 0.1 {
                            ops.push(Operation::WheelRotations(WheelRotations {
                                is_vertical: true,
                                rotation_units: (delta.y * 120.0) as i16,
                            }));
                        }
                    }
                    egui::Event::Key { key, pressed, .. } => {
                        // F11 is reserved for toggling Full Screen locally
                        if *key == egui::Key::F11 {
                            continue;
                        }
                        if let Some(sc) = egui_key_to_scancode(*key) {
                            if *pressed {
                                ops.push(Operation::KeyPressed(sc));
                            } else {
                                ops.push(Operation::KeyReleased(sc));
                            }
                        }
                    }
                    egui::Event::Text(text) => {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                ops.push(Operation::UnicodeKeyPressed(ch));
                                ops.push(Operation::UnicodeKeyReleased(ch));
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    ops
}

fn send_dirty_region(
    ui_tx: &Sender<RdpUiMessage>,
    image: &DecodedImage,
    dirty_bounds: Option<(u16, u16, u16, u16)>,
    force_full: bool,
) {
    let img_w = image.width();
    let img_h = image.height();
    if img_w == 0 || img_h == 0 {
        return;
    }

    if !force_full {
        if let Some((left, top, right, bottom)) = dirty_bounds {
            let x = left.min(img_w.saturating_sub(1));
            let y = top.min(img_h.saturating_sub(1));
            let r = right.min(img_w.saturating_sub(1));
            let b = bottom.min(img_h.saturating_sub(1));
            if r >= x && b >= y {
                let w = r - x + 1;
                let h = b - y + 1;
                let dirty_pixels = (w as usize) * (h as usize);
                let total_pixels = (img_w as usize) * (img_h as usize);

                // If dirty region is < 65% of screen, extract only the sub-rectangle
                if dirty_pixels * 100 < total_pixels * 65 {
                    let src = image.data();
                    let stride = (img_w as usize) * 4;
                    let row_bytes = (w as usize) * 4;
                    let mut sub_rgba = Vec::with_capacity(dirty_pixels * 4);
                    for row in (y as usize)..((y + h) as usize) {
                        let start = row * stride + (x as usize) * 4;
                        let end = start + row_bytes;
                        if end <= src.len() {
                            sub_rgba.extend_from_slice(&src[start..end]);
                        }
                    }
                    if sub_rgba.len() == dirty_pixels * 4 {
                        let _ = ui_tx.send(RdpUiMessage::PartialFrame {
                            total_width: img_w,
                            total_height: img_h,
                            x,
                            y,
                            w,
                            h,
                            rgba: sub_rgba,
                        });
                        return;
                    }
                }
            }
        }
    }

    let _ = ui_tx.send(RdpUiMessage::FullFrame {
        width: img_w,
        height: img_h,
        rgba: image.data().to_vec(),
    });
}

pub async fn start_rdp_session(
    profile: RdpProfile,
    initial_size: (u16, u16),
    ui_tx: Sender<RdpUiMessage>,
    mut input_rx: tokio::sync::mpsc::Receiver<RdpInputMessage>,
    ctx: egui::Context,
) {
    let addr_str = format!("{}:{}", profile.host, profile.port);
    let tcp_stream = match tokio::net::TcpStream::connect(&addr_str).await {
        Ok(s) => s,
        Err(e) => {
            let _ = ui_tx.send(RdpUiMessage::Error(format!("TCP connect error ({}): {}", addr_str, e)));
            ctx.request_repaint();
            return;
        }
    };

    // Critical latency optimization: Disable Nagle's algorithm so FastPath input & ACKs are sent immediately
    let _ = tcp_stream.set_nodelay(true);

    let client_addr = tcp_stream
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));

    let (init_w, init_h) = normalize_rdp_resolution(initial_size.0, initial_size.1);

    let perf_flags = if profile.fast_mode {
        PerformanceFlags::DISABLE_WALLPAPER
            | PerformanceFlags::DISABLE_FULLWINDOWDRAG
            | PerformanceFlags::DISABLE_MENUANIMATIONS
            | PerformanceFlags::DISABLE_CURSOR_SHADOW
            | PerformanceFlags::DISABLE_THEMING
            | PerformanceFlags::ENABLE_FONT_SMOOTHING
    } else {
        PerformanceFlags::default()
    };

    let config = Config {
        desktop_size: DesktopSize {
            width: init_w,
            height: init_h,
        },
        desktop_scale_factor: 100,
        enable_tls: true,
        enable_credssp: profile.enable_credssp,
        credentials: Credentials::UsernamePassword {
            username: profile.user.clone(),
            password: profile.password.clone(),
        },
        domain: if profile.domain.trim().is_empty() {
            None
        } else {
            Some(profile.domain.trim().to_string())
        },
        client_build: 0,
        client_name: "FaroCommander".to_string(),
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0x0407,
        ime_file_name: String::new(),
        bitmap: Some(BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs: BitmapCodecs::default(),
        }),
        dig_product_id: String::new(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_string(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        autologon: true,
        enable_audio_playback: false,
        performance_flags: perf_flags,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        enable_server_pointer: false,
        pointer_software_rendering: false,
    };

    // Attach DisplayControl dynamic virtual channel for live automatic resolution changes
    let mut connector = ClientConnector::new(config, client_addr).with_static_channel(
        DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new()))),
    );
    let mut framed = TokioFramed::new(tcp_stream);

    let should_upgrade = match connect_begin(&mut framed, &mut connector).await {
        Ok(u) => u,
        Err(e) => {
            let _ = ui_tx.send(RdpUiMessage::Error(format!("RDP handshake failed: {}", e)));
            ctx.request_repaint();
            return;
        }
    };

    let initial_stream = framed.into_inner_no_leftover();
    let (tls_stream, server_public_key) = match ironrdp_tls::upgrade(initial_stream, profile.host.as_str()).await {
        Ok(res) => res,
        Err(e) => {
            let _ = ui_tx.send(RdpUiMessage::Error(format!("TLS upgrade failed: {}", e)));
            ctx.request_repaint();
            return;
        }
    };

    let upgraded = mark_as_upgraded(should_upgrade, &mut connector);
    let mut tls_framed = TokioFramed::new(tls_stream);

    let connection_result = match connect_finalize(
        upgraded,
        &mut tls_framed,
        connector,
        ServerName::new(&profile.host),
        server_public_key,
        None,
        None,
    )
    .await
    {
        Ok(res) => res,
        Err(e) => {
            let _ = ui_tx.send(RdpUiMessage::Error(format!("RDP finalize / Auth error: {}", e)));
            ctx.request_repaint();
            return;
        }
    };

    let mut image = DecodedImage::new(
        PixelFormat::RgbA32,
        connection_result.desktop_size.width,
        connection_result.desktop_size.height,
    );
    let _ = ui_tx.send(RdpUiMessage::Connected {
        width: image.width(),
        height: image.height(),
    });
    ctx.request_repaint();

    let mut active_stage = ActiveStage::new(connection_result);
    let mut input_db = InputDatabase::new();

    loop {
        tokio::select! {
            pdu_res = tls_framed.read_pdu() => {
                let (action, frame) = match pdu_res {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = ui_tx.send(RdpUiMessage::Disconnected(format!("Stream closed: {}", e)));
                        ctx.request_repaint();
                        break;
                    }
                };

                let outputs = match active_stage.process(&mut image, action, &frame) {
                    Ok(o) => o,
                    Err(e) => {
                        let _ = ui_tx.send(RdpUiMessage::Error(format!("Session error: {}", e)));
                        ctx.request_repaint();
                        break;
                    }
                };

                let mut dirty_bounds: Option<(u16, u16, u16, u16)> = None;
                let mut force_full = false;

                for out in outputs {
                    match out {
                        ActiveStageOutput::ResponseFrame(resp) => {
                            if !resp.is_empty() {
                                let _ = tls_framed.write_all(&resp).await;
                            }
                        }
                        ActiveStageOutput::GraphicsUpdate(rect) => {
                            dirty_bounds = Some(match dirty_bounds {
                                Some((l, t, r, b)) => (
                                    l.min(rect.left),
                                    t.min(rect.top),
                                    r.max(rect.right),
                                    b.max(rect.bottom),
                                ),
                                None => (rect.left, rect.top, rect.right, rect.bottom),
                            });
                        }
                        ActiveStageOutput::Terminate(reason) => {
                            let _ = ui_tx.send(RdpUiMessage::Disconnected(reason.to_string()));
                            ctx.request_repaint();
                            return;
                        }
                        ActiveStageOutput::DeactivateAll(mut cas) => {
                            let mut buf = ironrdp::core::WriteBuf::new();
                            while !matches!(cas.state, ConnectionActivationState::Finalized { .. }) {
                                if single_sequence_step(&mut tls_framed, &mut *cas, &mut buf).await.is_err() {
                                    break;
                                }
                            }
                            if let ConnectionActivationState::Finalized {
                                desktop_size,
                                enable_server_pointer,
                                pointer_software_rendering,
                                io_channel_id,
                                user_channel_id,
                            } = cas.state
                            {
                                image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
                                active_stage.set_fastpath_processor(
                                    ironrdp::session::fast_path::ProcessorBuilder {
                                        io_channel_id,
                                        user_channel_id,
                                        enable_server_pointer,
                                        pointer_software_rendering,
                                    }
                                    .build(),
                                );
                                active_stage.set_enable_server_pointer(enable_server_pointer);
                                let _ = ui_tx.send(RdpUiMessage::Connected {
                                    width: desktop_size.width,
                                    height: desktop_size.height,
                                });
                                force_full = true;
                            }
                        }
                        _ => {}
                    }
                }

                if force_full || dirty_bounds.is_some() {
                    send_dirty_region(&ui_tx, &image, dirty_bounds, force_full);
                    ctx.request_repaint();
                }
            }
            input_opt = input_rx.recv() => {
                match input_opt {
                    Some(first_msg) => {
                        let mut ops_batch = Vec::new();
                        let mut pending_resize: Option<(u16, u16)> = None;
                        let mut should_disconnect = false;

                        match first_msg {
                            RdpInputMessage::Operations(ops) => ops_batch.extend(ops),
                            RdpInputMessage::Resize { width, height } => pending_resize = Some((width, height)),
                            RdpInputMessage::Disconnect => should_disconnect = true,
                        }

                        // Drain any queued input messages to coalesce mouse movement bursts
                        while let Ok(next_msg) = input_rx.try_recv() {
                            match next_msg {
                                RdpInputMessage::Operations(ops) => ops_batch.extend(ops),
                                RdpInputMessage::Resize { width, height } => pending_resize = Some((width, height)),
                                RdpInputMessage::Disconnect => {
                                    should_disconnect = true;
                                    break;
                                }
                            }
                        }

                        if should_disconnect {
                            if let Ok(outputs) = active_stage.graceful_shutdown() {
                                for out in outputs {
                                    if let ActiveStageOutput::ResponseFrame(resp) = out {
                                        let _ = tls_framed.write_all(&resp).await;
                                    }
                                }
                            }
                            use tokio::io::AsyncWriteExt;
                            let (mut stream, _) = tls_framed.into_inner();
                            let _ = stream.shutdown().await;
                            let _ = ui_tx.send(RdpUiMessage::Disconnected("Sitzung beendet".to_string()));
                            ctx.request_repaint();
                            break;
                        }

                        if !ops_batch.is_empty() {
                            let events = input_db.apply(ops_batch);
                            if !events.is_empty() {
                                if let Ok(outputs) = active_stage.process_fastpath_input(&mut image, &events) {
                                    for out in outputs {
                                        if let ActiveStageOutput::ResponseFrame(resp) = out {
                                            if !resp.is_empty() {
                                                let _ = tls_framed.write_all(&resp).await;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if let Some((req_w, req_h)) = pending_resize {
                            let (adj_w, adj_h) = normalize_rdp_resolution(req_w, req_h);
                            if adj_w != image.width() || adj_h != image.height() {
                                if let Some(Ok(encoded)) = active_stage.encode_resize(adj_w as u32, adj_h as u32, None, None) {
                                    let _ = tls_framed.write_all(&encoded).await;
                                }
                            }
                        }
                    }
                    None => {
                        if let Ok(outputs) = active_stage.graceful_shutdown() {
                            for out in outputs {
                                if let ActiveStageOutput::ResponseFrame(resp) = out {
                                    let _ = tls_framed.write_all(&resp).await;
                                }
                            }
                        }
                        use tokio::io::AsyncWriteExt;
                        let (mut stream, _) = tls_framed.into_inner();
                        let _ = stream.shutdown().await;
                        break;
                    }
                }
            }
        }
    }
}
