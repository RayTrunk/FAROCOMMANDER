use std::sync::mpsc::Sender;
use netscli_core::{
    default_ipv4_subnet_string, dns::DnsRecord, parse_ports_checked, ArpEntry, DiscoverPhase,
    DiscoverProgress, Host, InspectResult, InterfaceInfo, MdnsService, Ops, OpsConfig, PingSummary,
    PortResult, PortScanProgress, SweepEntry, SweepPhase, SweepProgress, TraceResult,
};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

fn spawn_async<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Runtime::new() {
            rt.block_on(fut);
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetScannerTab {
    #[default]
    Discover,
    Inspect,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagTool {
    #[default]
    Ping,
    Traceroute,
    Dns,
    ArpAndInterfaces,
    Mdns,
}

#[derive(Debug)]
pub enum NetScanUiMessage {
    Progress {
        text: String,
        fraction: Option<f32>,
    },
    DiscoverDone {
        subnet: String,
        entries: Vec<SweepEntry>,
    },
    InspectDone(Box<InspectResult>),
    PortScanDone {
        host: String,
        ip: String,
        ports: Vec<PortResult>,
    },
    PingDone(PingSummary),
    TraceDone(TraceResult),
    DnsDone {
        host: String,
        records: Vec<DnsRecord>,
    },
    ArpDone {
        arp: Vec<ArpEntry>,
        interfaces: Vec<InterfaceInfo>,
    },
    MdnsDone(Vec<MdnsService>),
    Error(String),
}

pub struct NetScannerState {
    pub active_tab: NetScannerTab,
    pub running: bool,
    pub status_text: String,
    pub progress_fraction: Option<f32>,
    pub error_message: Option<String>,

    // Config
    pub concurrency: usize,
    pub scan_timeout_ms: u64,
    pub ping_timeout_ms: u64,

    // Tab 1: Subnet Discover / Sweep
    pub subnet_input: String,
    pub resolve_hostnames: bool,
    pub with_port_sweep: bool,
    pub sweep_ports_input: String,
    pub filter_query: String,
    pub only_with_open_ports: bool,
    pub discovered_entries: Vec<SweepEntry>,

    // Tab 2: Host Inspect & Port Scan
    pub target_host: String,
    pub scan_ports_input: String,
    pub use_udp: bool,
    pub show_closed_ports: bool,
    pub inspect_result: Option<InspectResult>,

    // Tab 3: Diagnostics
    pub diag_tool: DiagTool,
    pub diag_target: String,
    pub ping_count: u32,
    pub trace_max_hops: u32,
    pub dns_record_type: String,
    pub mdns_timeout_secs: u64,
    pub ping_result: Option<PingSummary>,
    pub trace_result: Option<TraceResult>,
    pub dns_records: Vec<DnsRecord>,
    pub arp_entries: Vec<ArpEntry>,
    pub interfaces: Vec<InterfaceInfo>,
    pub mdns_services: Vec<MdnsService>,

    // CSV Save Dialog
    pub show_export_dialog: bool,
    pub export_path_input: String,
}

impl Default for NetScannerState {
    fn default() -> Self {
        let interfaces = Ops::default().list_interfaces();
        Self {
            active_tab: NetScannerTab::Discover,
            running: false,
            status_text: "Bereit".to_string(),
            progress_fraction: None,
            error_message: None,
            concurrency: netscli_core::DEFAULT_CONCURRENCY,
            scan_timeout_ms: netscli_core::DEFAULT_SCAN_TIMEOUT_MS,
            ping_timeout_ms: netscli_core::DEFAULT_PING_TIMEOUT_MS,
            subnet_input: default_ipv4_subnet_string(),
            resolve_hostnames: true,
            with_port_sweep: false,
            sweep_ports_input: "21,22,80,135,139,443,445,3389,5900,8080".to_string(),
            filter_query: String::new(),
            only_with_open_ports: false,
            discovered_entries: Vec::new(),
            target_host: String::new(),
            scan_ports_input: String::new(),
            use_udp: false,
            show_closed_ports: false,
            inspect_result: None,
            diag_tool: DiagTool::Ping,
            diag_target: "1.1.1.1".to_string(),
            ping_count: 4,
            trace_max_hops: 20,
            dns_record_type: "ALL".to_string(),
            mdns_timeout_secs: 4,
            ping_result: None,
            trace_result: None,
            dns_records: Vec::new(),
            arp_entries: Vec::new(),
            interfaces,
            mdns_services: Vec::new(),
            show_export_dialog: false,
            export_path_input: String::new(),
        }
    }
}

impl NetScannerState {
    pub fn ops(&self) -> Ops {
        Ops::new(OpsConfig {
            concurrency: self.concurrency,
            scan_timeout_ms: self.scan_timeout_ms,
            ping_timeout_ms: self.ping_timeout_ms,
            dns_timeout_ms: netscli_core::DEFAULT_DNS_TIMEOUT_MS,
        })
    }

    pub fn parse_optional_ports(input: &str) -> Result<Option<Vec<u16>>, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            parse_ports_checked(Some(trimmed)).map_err(|e| e.to_string())
        }
    }

    pub fn start_discover(&mut self, tx: Sender<NetScanUiMessage>, ctx: egui::Context) {
        let ports = if self.with_port_sweep {
            match Self::parse_optional_ports(&self.sweep_ports_input) {
                Ok(p) => p,
                Err(e) => {
                    self.error_message = Some(format!("Ungültige Portliste: {e}"));
                    return;
                }
            }
        } else {
            None
        };

        self.running = true;
        self.error_message = None;
        self.progress_fraction = Some(0.0);
        self.status_text = format!("Scanne Subnetz {}...", self.subnet_input);

        let ops = self.ops();
        let subnet = {
            let s = self.subnet_input.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        };
        let resolve = self.resolve_hostnames;
        let with_sweep = self.with_port_sweep;

        spawn_async(async move {
            if with_sweep {
                let tx_prog = tx.clone();
                let ctx_prog = ctx.clone();
                let cb = Arc::new(move |p: SweepProgress| {
                    let phase_str = match p.phase {
                        SweepPhase::DiscoverPing => "Ping-Sweep",
                        SweepPhase::DiscoverResolve => "DNS/ARP-Auflösung",
                        SweepPhase::Scan => "Port-Scan",
                    };
                    let frac = if p.total > 0 {
                        Some(p.completed as f32 / p.total as f32)
                    } else {
                        None
                    };
                    let _ = tx_prog.send(NetScanUiMessage::Progress {
                        text: format!(
                            "{}: {}/{} ({} aktiv) - {}",
                            phase_str, p.completed, p.total, p.found, p.ip
                        ),
                        fraction: frac,
                    });
                    ctx_prog.request_repaint();
                });

                match ops.sweep_ipv4_with_progress(subnet, ports, resolve, Some(cb)).await {
                    Ok((subnet, entries)) => {
                        let _ = tx.send(NetScanUiMessage::DiscoverDone { subnet, entries });
                    }
                    Err(e) => {
                        let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                    }
                }
            } else {
                let tx_prog = tx.clone();
                let ctx_prog = ctx.clone();
                let cb = Arc::new(move |p: DiscoverProgress| {
                    let phase_str = match p.phase {
                        DiscoverPhase::Ping => "Ping-Sweep",
                        DiscoverPhase::Resolve => "DNS/ARP-Auflösung",
                    };
                    let frac = if p.total > 0 {
                        Some(p.completed as f32 / p.total as f32)
                    } else {
                        None
                    };
                    let _ = tx_prog.send(NetScanUiMessage::Progress {
                        text: format!(
                            "{}: {}/{} ({} gefunden) - {}",
                            phase_str, p.completed, p.total, p.found, p.ip
                        ),
                        fraction: frac,
                    });
                    ctx_prog.request_repaint();
                });

                match ops.discover_ipv4_with_progress(subnet, resolve, Some(cb)).await {
                    Ok((subnet, hosts)) => {
                        let entries = hosts
                            .into_iter()
                            .map(|host: Host| SweepEntry {
                                host,
                                open_ports: Vec::new(),
                            })
                            .collect();
                        let _ = tx.send(NetScanUiMessage::DiscoverDone { subnet, entries });
                    }
                    Err(e) => {
                        let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    pub fn start_inspect(
        &mut self,
        tx: Sender<NetScanUiMessage>,
        ctx: egui::Context,
        only_ports: bool,
    ) {
        let host = self.target_host.trim().to_string();
        if host.is_empty() {
            self.error_message = Some("Bitte Host oder IP-Adresse eingeben.".to_string());
            return;
        }
        let ports = match Self::parse_optional_ports(&self.scan_ports_input) {
            Ok(p) => p,
            Err(e) => {
                self.error_message = Some(format!("Ungültige Portliste: {e}"));
                return;
            }
        };

        self.running = true;
        self.error_message = None;
        self.progress_fraction = None;
        self.status_text = format!("Untersuche Host {}...", host);

        let ops = self.ops();
        let use_udp = self.use_udp;

        spawn_async(async move {
            if only_ports || use_udp {
                let tx_prog = tx.clone();
                let ctx_prog = ctx.clone();
                let cb = Arc::new(move |p: PortScanProgress| {
                    let frac = if p.total > 0 {
                        Some(p.completed as f32 / p.total as f32)
                    } else {
                        None
                    };
                    let _ = tx_prog.send(NetScanUiMessage::Progress {
                        text: format!(
                            "Port-Scan: {}/{} Ports ({} offen) - Port {}",
                            p.completed, p.total, p.open_found, p.port
                        ),
                        fraction: frac,
                    });
                    ctx_prog.request_repaint();
                });

                let res = if use_udp {
                    ops.scan_udp_ports_with_progress(&host, ports, Some(cb)).await
                } else {
                    ops.scan_ports_with_progress(&host, ports, Some(cb)).await
                };

                match res {
                    Ok((ip, ports)) => {
                        let _ = tx.send(NetScanUiMessage::PortScanDone {
                            host,
                            ip: ip.to_string(),
                            ports,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                    }
                }
            } else {
                match ops.inspect_host(host, ports).await {
                    Ok(res) => {
                        let _ = tx.send(NetScanUiMessage::InspectDone(Box::new(res)));
                    }
                    Err(e) => {
                        let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    pub fn start_diagnostics(&mut self, tx: Sender<NetScanUiMessage>, ctx: egui::Context) {
        let tool = self.diag_tool;
        let target = self.diag_target.trim().to_string();
        if matches!(tool, DiagTool::Ping | DiagTool::Traceroute | DiagTool::Dns) && target.is_empty() {
            self.error_message = Some("Bitte Ziel-Host oder IP eingeben.".to_string());
            return;
        }

        self.running = true;
        self.error_message = None;
        self.progress_fraction = None;
        let ops = self.ops();
        let ping_count = self.ping_count;
        let trace_hops = self.trace_max_hops;
        let resolve = self.resolve_hostnames;
        let dns_type = self.dns_record_type.clone();
        let mdns_secs = self.mdns_timeout_secs;

        self.status_text = match tool {
            DiagTool::Ping => format!("Pinge {} ({} Pakete)...", target, ping_count),
            DiagTool::Traceroute => format!("Traceroute zu {} (max {} Hops)...", target, trace_hops),
            DiagTool::Dns => format!("DNS-Abfrage ({}) für {}...", dns_type, target),
            DiagTool::ArpAndInterfaces => "Lese ARP-Tabelle & Netzwerkschnittstellen...".to_string(),
            DiagTool::Mdns => format!("Suche mDNS/Bonjour-Dienste ({}s)...", mdns_secs),
        };

        spawn_async(async move {
            match tool {
                DiagTool::Ping => match ops.ping_host_summary(&target, ping_count).await {
                    Ok(res) => {
                        let _ = tx.send(NetScanUiMessage::PingDone(res));
                    }
                    Err(e) => {
                        let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                    }
                },
                DiagTool::Traceroute => {
                    let (prog_tx, mut prog_rx) = tokio::sync::watch::channel(String::new());
                    let tx_prog = tx.clone();
                    let ctx_prog = ctx.clone();
                    tokio::spawn(async move {
                        while prog_rx.changed().await.is_ok() {
                            let line = prog_rx.borrow().clone();
                            if !line.is_empty() {
                                let _ = tx_prog.send(NetScanUiMessage::Progress {
                                    text: format!("Traceroute: {line}"),
                                    fraction: None,
                                });
                                ctx_prog.request_repaint();
                            }
                        }
                    });
                    match ops
                        .trace_route_with_progress(&target, trace_hops, resolve, Some(prog_tx))
                        .await
                    {
                        Ok(res) => {
                            let _ = tx.send(NetScanUiMessage::TraceDone(res));
                        }
                        Err(e) => {
                            let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                        }
                    }
                }
                DiagTool::Dns => {
                    if dns_type.eq_ignore_ascii_case("PTR") {
                        match ops.reverse_lookup(&target).await {
                            Ok(opt) => {
                                let records = opt
                                    .into_iter()
                                    .map(|val| DnsRecord {
                                        record_type: "PTR".to_string(),
                                        value: val,
                                        name: Some(target.clone()),
                                        ttl_seconds: None,
                                        resolver_source: Some("reverse".to_string()),
                                    })
                                    .collect();
                                let _ = tx.send(NetScanUiMessage::DnsDone {
                                    host: target,
                                    records,
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                            }
                        }
                    } else {
                        match ops.dns_lookup(&target, Some(dns_type)).await {
                            Ok(records) => {
                                let _ = tx.send(NetScanUiMessage::DnsDone {
                                    host: target,
                                    records,
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                            }
                        }
                    }
                }
                DiagTool::ArpAndInterfaces => {
                    let interfaces = ops.list_interfaces();
                    match ops.get_arp_table().await {
                        Ok(arp) => {
                            let _ = tx.send(NetScanUiMessage::ArpDone { arp, interfaces });
                        }
                        Err(e) => {
                            let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                        }
                    }
                }
                DiagTool::Mdns => {
                    match ops
                        .discover_mdns(&[], Duration::from_secs(mdns_secs.clamp(1, 15)))
                        .await
                    {
                        Ok(services) => {
                            let _ = tx.send(NetScanUiMessage::MdnsDone(services));
                        }
                        Err(e) => {
                            let _ = tx.send(NetScanUiMessage::Error(e.to_string()));
                        }
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    pub fn default_csv_filename(&self) -> String {
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let prefix = match self.active_tab {
            NetScannerTab::Discover => "netscan_subnet",
            NetScannerTab::Inspect => "netscan_host",
            NetScannerTab::Diagnostics => match self.diag_tool {
                DiagTool::Ping => "netscan_ping",
                DiagTool::Traceroute => "netscan_trace",
                DiagTool::Dns => "netscan_dns",
                DiagTool::ArpAndInterfaces => "netscan_arp",
                DiagTool::Mdns => "netscan_mdns",
            },
        };
        format!("{prefix}_{stamp}.csv")
    }

    pub fn pick_save_csv_path(initial_path: &str) -> Option<String> {
        #[cfg(windows)]
        {
            let escaped = initial_path.replace('\'', "''");
            let script = format!(
                "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.SaveFileDialog; $d.Filter = 'CSV-Datei (*.csv)|*.csv|Alle Dateien (*.*)|*.*'; $d.FileName = '{}'; if ($d.ShowDialog() -eq 'OK') {{ Write-Output $d.FileName }}",
                escaped
            );
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Sta", "-WindowStyle", "Hidden", "-Command", &script])
                .output()
                .ok()?;
            let picked = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if picked.is_empty() { None } else { Some(picked) }
        }
        #[cfg(not(windows))]
        {
            // Try zenity or kdialog if present on Linux desktop
            if let Ok(out) = std::process::Command::new("zenity")
                .args(["--file-selection", "--save", "--confirm-overwrite", &format!("--filename={}", initial_path)])
                .output()
            {
                let picked = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !picked.is_empty() {
                    return Some(picked);
                }
            }
            if let Ok(out) = std::process::Command::new("kdialog")
                .args(["--getsavefilename", initial_path, "*.csv|CSV Files"])
                .output()
            {
                let picked = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !picked.is_empty() {
                    return Some(picked);
                }
            }
            Some(initial_path.to_string())
        }
    }

    pub fn export_csv_to_path(&self, raw_path: &std::path::Path) -> Result<std::path::PathBuf, String> {
        fn cell(s: &str) -> String {
            format!("\"{}\"", s.replace('"', "\"\""))
        }

        let mut out = String::from("\u{FEFF}");

        match self.active_tab {
            NetScannerTab::Discover => {
                if self.discovered_entries.is_empty() {
                    return Err("Keine Subnetz-Ergebnisse zum Exportieren vorhanden.".to_string());
                }
                out.push_str("IP,Hostname,Quelle_Name,MAC,Hersteller,RTT_ms,Gefunden_Durch,Offene_Ports\r\n");
                for e in &self.discovered_entries {
                    let h = &e.host;
                    let ports = e
                        .open_ports
                        .iter()
                        .map(|p| match &p.service {
                            Some(s) => format!("{}/{}", p.port, s),
                            None => p.port.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    let src = match h.hostname_source {
                        Some(netscli_core::NameSource::Mdns) => "mDNS",
                        Some(netscli_core::NameSource::Reverse) => "DNS",
                        None => "",
                    };
                    let found_by = match h.found_by {
                        netscli_core::FoundBy::Probe => "Probe",
                        netscli_core::FoundBy::Neighbor => "ARP",
                    };
                    out.push_str(&format!(
                        "{},{},{},{},{},{},{},{}\r\n",
                        cell(&h.ip.to_string()),
                        cell(h.hostname.as_deref().unwrap_or("")),
                        cell(src),
                        cell(h.mac.as_deref().unwrap_or("")),
                        cell(h.vendor.as_deref().unwrap_or("")),
                        h.rtt_ms.map(|v| v.to_string()).unwrap_or_default(),
                        cell(found_by),
                        cell(&ports),
                    ));
                }
            }
            NetScannerTab::Inspect => {
                let Some(res) = &self.inspect_result else {
                    return Err("Keine Host-Inspektionsergebnisse zum Exportieren vorhanden.".to_string());
                };
                out.push_str("Host,IP,Hostname,MAC,Hersteller,OS_Erkennung,Port,Protokoll,Status,Dienst,Produkt_Version,Latenz_ms,Details\r\n");
                let ip_s = res.ip.map(|i| i.to_string()).unwrap_or_default();
                let os_s = res.os_hint.as_ref().map(|o| o.summary()).unwrap_or_default();
                for p in &res.ports {
                    if !self.show_closed_ports && !p.open {
                        continue;
                    }
                    let proto = match p.protocol {
                        netscli_core::Protocol::Tcp => "TCP",
                        netscli_core::Protocol::Udp => "UDP",
                    };
                    let status = match p.status {
                        netscli_core::PortStatus::Open => "OPEN",
                        netscli_core::PortStatus::OpenFiltered => "OPEN|FILTERED",
                        netscli_core::PortStatus::Filtered => "FILTERED",
                        netscli_core::PortStatus::Closed => "CLOSED",
                        netscli_core::PortStatus::Error => "ERROR",
                    };
                    let mut details = Vec::new();
                    if let Some(b) = &p.banner {
                        details.push(format!("Banner: {b}"));
                    }
                    if let Some(http) = &p.http {
                        if let Some(st) = &http.status_line {
                            details.push(format!("HTTP: {st}"));
                        }
                    }
                    if let Some(tls) = &p.tls {
                        details.push(format!(
                            "TLS: {} {}",
                            tls.protocol.as_deref().unwrap_or(""),
                            tls.cipher_suite.as_deref().unwrap_or("")
                        ));
                    }
                    out.push_str(&format!(
                        "{},{},{},{},{},{},{},{},{},{},{},{},{}\r\n",
                        cell(&res.host),
                        cell(&ip_s),
                        cell(res.hostname.as_deref().unwrap_or("")),
                        cell(res.mac.as_deref().unwrap_or("")),
                        cell(res.vendor.as_deref().unwrap_or("")),
                        cell(&os_s),
                        p.port,
                        proto,
                        status,
                        cell(p.service.as_deref().unwrap_or("")),
                        cell(&p.product_and_version().unwrap_or_default()),
                        p.latency_ms.map(|v| v.to_string()).unwrap_or_default(),
                        cell(&details.join(" | ")),
                    ));
                }
            }
            NetScannerTab::Diagnostics => match self.diag_tool {
                DiagTool::Ping => {
                    let Some(p) = &self.ping_result else {
                        return Err("Kein Ping-Ergebnis zum Exportieren vorhanden.".to_string());
                    };
                    out.push_str("Host,IP,Gesendet,Empfangen,Verlust_Prozent,Min_ms,Avg_ms,Max_ms\r\n");
                    out.push_str(&format!(
                        "{},{},{},{},{:.1},{},{},{}\r\n",
                        cell(&p.host),
                        cell(&p.ip.to_string()),
                        p.sent,
                        p.received,
                        p.loss_pct,
                        p.rtt_ms_min.map(|v| v.to_string()).unwrap_or_default(),
                        p.rtt_ms_avg.map(|v| format!("{v:.1}")).unwrap_or_default(),
                        p.rtt_ms_max.map(|v| v.to_string()).unwrap_or_default(),
                    ));
                }
                DiagTool::Traceroute => {
                    let Some(tr) = &self.trace_result else {
                        return Err("Kein Traceroute-Ergebnis zum Exportieren vorhanden.".to_string());
                    };
                    out.push_str("Zeile,Ausgabe\r\n");
                    for (i, line) in tr.lines.iter().enumerate() {
                        out.push_str(&format!("{},{}\r\n", i + 1, cell(line)));
                    }
                }
                DiagTool::Dns => {
                    if self.dns_records.is_empty() {
                        return Err("Keine DNS-Einträge zum Exportieren vorhanden.".to_string());
                    }
                    out.push_str("Typ,Name,Wert,TTL_Sekunden\r\n");
                    for r in &self.dns_records {
                        out.push_str(&format!(
                            "{},{},{},{}\r\n",
                            cell(&r.record_type),
                            cell(r.name.as_deref().unwrap_or("")),
                            cell(&r.value),
                            r.ttl_seconds.map(|v| v.to_string()).unwrap_or_default(),
                        ));
                    }
                }
                DiagTool::ArpAndInterfaces => {
                    out.push_str("IP,MAC,Hersteller,Interface\r\n");
                    for a in &self.arp_entries {
                        out.push_str(&format!(
                            "{},{},{},{}\r\n",
                            cell(&a.ip.to_string()),
                            cell(&a.mac.to_string()),
                            cell(a.vendor.as_deref().unwrap_or("")),
                            cell(&a.interface),
                        ));
                    }
                }
                DiagTool::Mdns => {
                    if self.mdns_services.is_empty() {
                        return Err("Keine mDNS-Dienste zum Exportieren vorhanden.".to_string());
                    }
                    out.push_str("Name,Dienst_Typ,Hostname,Port,IP_Adressen\r\n");
                    for s in &self.mdns_services {
                        let ips = s
                            .addresses
                            .iter()
                            .map(|a| a.to_string())
                            .collect::<Vec<_>>()
                            .join("; ");
                        out.push_str(&format!(
                            "{},{},{},{},{}\r\n",
                            cell(&s.full_name),
                            cell(&s.service_type),
                            cell(&s.hostname),
                            s.port,
                            cell(&ips),
                        ));
                    }
                }
            },
        }

        let path = if raw_path.is_dir() {
            raw_path.join(self.default_csv_filename())
        } else if raw_path.extension().is_none() {
            raw_path.with_extension("csv")
        } else {
            raw_path.to_path_buf()
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, out.as_bytes()).map_err(|e| format!("Fehler beim Speichern: {e}"))?;
        Ok(path)
    }
}
