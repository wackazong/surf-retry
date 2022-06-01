# surf-retry

A retry middleware for surf

## Install

With [cargo add](https://github.com/killercup/cargo-edit#Installation) installed :

```sh
cargo add surf-retry
```

## Documentation

- [API Docs](https://docs.rs/surf-retry)

## Example

 ```rust
 use surf_retry::RetryMiddleware;
 use surf::{Client, Request, http::Method};
 use url::Url;

 use std::time::Duration;

 #[async_std::main]
 async fn main() -> surf::Result<()> {
     let req = Request::new(Method::Get, Url::parse("https://example.api")?);
     let client = Client::new().with(RetryMiddleware::new(
            3,
            ExponentialBackoff::builder().build_with_total_retry_duration(Duration::from_secs(60)),
            1,
        ));
     let res = client.send(req).await?;
     Ok(())
 }
 ```
