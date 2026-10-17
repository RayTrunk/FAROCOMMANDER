pub enum PaneMessage {
    SftpConnected(russh_sftp::client::SftpSession),
    OpendalConnected(opendal::Operator),
    SftpError(String),
    FilesFetched(Vec<crate::FileEntry>),
    DirSizeCalculated(String, u64),
    Refresh,
}

pub enum SshMessage {
    Output(String),
    Error(String),
    Disconnected,
}

pub enum EditorMessage {
    FileLoaded(String, String, String),
    ImageLoaded(String, [usize; 2], Vec<u8>),
    Error(String),
}

