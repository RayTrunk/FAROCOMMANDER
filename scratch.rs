fn check() { let url = url::Url::parse(\
ssh://root@192.168.1.1\); println!(\
:?
\, url.is_ok()); }
