use opendal::Operator;
use opendal::services::Ftp;

#[tokio::main]
async fn main() {
    let mut builder = Ftp::default();
    builder = builder.endpoint("test.rebex.net:21");
    builder = builder.user("demo");
    builder = builder.password("password");
    
    let op: Operator = Operator::new(builder).unwrap().into();
    match op.stat("/").await {
        Ok(_) => println!("Connected and stat / OK"),
        Err(e) => println!("Error: {:?}", e),
    }
}
