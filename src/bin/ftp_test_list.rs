use opendal::Operator;
use opendal::services::Ftp;
use futures::TryStreamExt;

#[tokio::main]
async fn main() {
    let mut builder = Ftp::default();
    builder = builder.endpoint("test.rebex.net:21");
    builder = builder.user("demo");
    builder = builder.password("password");
    
    let op: Operator = Operator::new(builder).unwrap().into();
    
    let path = "/";
    let mut lister = match op.lister(path).await {
        Ok(l) => l,
        Err(e) => {
            println!("Lister error: {:?}", e);
            return;
        }
    };
    
    while let Some(entry) = lister.try_next().await.unwrap_or(None) {
        println!("Entry: {}", entry.name());
    }
}
