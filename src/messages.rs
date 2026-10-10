pub enum PaneMessage {
    SftpConnected(russh_sftp::client::SftpSession),
    OpendalConnected(opendal::Operator),
    SftpError(String),
    FilesFetched(Vec<crate::FileEntry>),
}

pub enum SshMessage {
    Output(String),
    Error(String),
    Disconnected,
}
