use url::Url;

fn main() {
    let test_cases = vec![
        "192.168.1.1",
        "user@192.168.1.1",
        "user:pass@192.168.1.1:2222",
    ];
    for tc in test_cases {
        let uri = format!("ssh://{}", tc);
        match Url::parse(&uri) {
            Ok(url) => println!("{} -> host: {:?}, port: {:?}, user: {:?}, pass: {:?}", tc, url.host_str(), url.port(), url.username(), url.password()),
            Err(e) => println!("{} -> Error: {}", tc, e),
        }
        
        let uri2 = format!("sftp://{}", tc);
        match Url::parse(&uri2) {
            Ok(url) => println!("{} -> host: {:?}, port: {:?}, user: {:?}, pass: {:?}", tc, url.host_str(), url.port(), url.username(), url.password()),
            Err(e) => println!("{} -> Error: {}", tc, e),
        }
    }
}
