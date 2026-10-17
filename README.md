# FaroCommander

Moderne Zwei-Fenster-Dateiverwaltung (Dual-Pane File Manager) für Windows in Rust (`eframe` / `egui`).

## Features

- **Dual-Pane Dateimanager**: Schnelle Navigation, Tabs/Laufwerksauswahl, Favoriten (`Strg+D`), Verzeichnisgrößenberechnung und Ordner-Synchronisation.
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

## Lizenz

Veröffentlicht unter der [MIT License](LICENSE) – Copyright (c) 2026 R.Trunk.
