# FaroCommander

**Modern Dual-Pane File Manager for Windows built in Rust (`eframe` / `egui`).**  
**Moderner Zwei-Fenster-Dateimanager für Windows, entwickelt in Rust (`eframe` / `egui`).**

---

## 🇬🇧 English

### Features

- **Dual-Pane File Manager**: Fast keyboard & mouse navigation, drive selector, bookmarks (`Ctrl+D`), folder size calculation, and directory synchronization.
- **Built-in Remote & Network Clients**:
  - **SFTP & SSH Terminal** (`russh` / `russh-sftp`) with an AES-256-GCM encrypted profile vault (PBKDF2-HMAC-SHA256).
  - **FTP & WebDAV** (`opendal`).
  - **Remote Desktop (RDP)** client (`ironrdp`) embedded directly inside the application window.
- **Archive Support**: Browse, pack, and extract `ZIP`, `7z`, `TAR`, `TAR.GZ`, `TAR.BZ2`, and `TAR.XZ` archives transparently.
- **Integrated Tools**:
  - **Network Scanner** (`netscli-core`): Subnet/IP-range scanning, port scanning (Quick / Common / Full), service banner grabbing, mDNS/DNS resolution, MAC vendor lookup, and CSV export.
  - **Password & Passphrase Generator**: Character passwords, dictionary passphrases (e.g. `ARCTIC-WOLF-XDFG`), and PINs with real-time entropy and crack-time estimation (`zxcvbn`).
  - **Multi-Rename Tool**, **File Properties & Hash Verification** (`CRC32`, `MD5`, `SHA-256`), and **Built-in Code/Text Viewer & Editor** with syntax highlighting (`F3` / `F4`).
- **Interface**: English & German UI, 5 themes (Dark, Light, Classic Blue, Midnight, Solarized Dark).

---

## 🇩🇪 Deutsch

### Funktionen

- **Dual-Pane Dateimanager**: Schnelle Tastatur- und Mausnavigation, Laufwerksauswahl, Favoriten (`Strg+D`), Verzeichnisgrößenberechnung und Ordner-Synchronisation.
- **Integrierte Netzwerk-Clients**:
  - **SFTP & SSH-Terminal** (`russh` / `russh-sftp`) mit AES-256-GCM verschlüsseltem Profil-Tresor (PBKDF2-HMAC-SHA256).
  - **FTP & WebDAV** (`opendal`).
  - **Remote Desktop (RDP)** Client (`ironrdp`) direkt im Anwendungsfenster.
- **Archiv-Unterstützung**: Transparentes Öffnen, Packen und Entpacken von `ZIP`, `7z`, `TAR`, `TAR.GZ`, `TAR.BZ2` und `TAR.XZ`.
- **Integrierte Tools**:
  - **Netzwerk-Scanner** (`netscli-core`): Subnetz-/IP-Range-Scan, Port-Scan (Quick / Common / Full), Dienst-Banner-Erkennung, mDNS/DNS-Auflösung, MAC-Hersteller-Lookup und CSV-Export.
  - **Passwort- & Passphrasen-Generator**: Zeichen-Passwörter, Wörterbuch-Passphrasen (z. B. `ARCTIC-WOLF-XDFG`) und PINs inkl. Echtzeit-Entropie- und Crack-Zeit-Analyse (`zxcvbn`).
  - **Mehrfach-Umbenennung (Multi-Rename-Tool)**, **Datei-Eigenschaften & Hash-Prüfung** (`CRC32`, `MD5`, `SHA-256`), sowie **integrierter Code-/Text-Editor** mit Syntax-Highlighting (`F3` / `F4`).
- **Oberfläche**: Deutsch & Englisch, 5 Farbschemata (Dark, Light, Classic Blue, Midnight, Solarized Dark).

---

## License / Lizenz

Published under the [MIT License](LICENSE) – Copyright (c) 2026 R.Trunk.
