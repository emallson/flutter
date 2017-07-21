extern crate hyper;
extern crate hyper_tls;
extern crate tokio_core;
#[macro_use]
extern crate serde_derive;
extern crate futures;
extern crate serde_json;
extern crate serde;
extern crate base64;
#[macro_use]
extern crate error_chain;

pub mod errors;
use errors::*;

use tokio_core::reactor::Core;
use hyper::{Method, Client, Request, StatusCode};
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use serde_json::Value;
use futures::{Stream, Future};

#[derive(Serialize, Deserialize)]
pub struct ApplicationAuth {
    key: String,
    secret: String,
}

pub struct Twitter {
    client: Client<HttpsConnector<HttpConnector>>,
    bearer: String,
}

impl Twitter {
    /// Construct a `Twitter`.
    ///
    /// Note that this function *blocks* since returning the future to complete the request is
    /// actually an enormous pain.
    pub fn authenticate_with(core: &mut Core, auth: ApplicationAuth) -> Result<Self> {
        use hyper::header::{Authorization, Basic, ContentType};
        use hyper::mime::Mime;
        use std::str::FromStr;

        let handle = core.handle();
        let client = Client::configure()
            .connector(HttpsConnector::new(4, &handle).unwrap())
            .build(&handle);

        let uri = "https://api.twitter.com/oauth2/token".parse()?;

        let mut req = Request::new(Method::Post, uri);

        {
            let mut headers = req.headers_mut();
            headers.set(Authorization(Basic {
                username: auth.key,
                password: Some(auth.secret),
            }));
            headers.set(ContentType(Mime::from_str("application/x-www-form-urlencoded; \
                                                   charset=utf-8")
                .unwrap()));
        }
        req.set_body("grant_type=client_credentials");

        let work = client.request(req)
            .and_then(|res| {
                let stat = res.status();
                res.body()
                    .concat2()
                    .and_then(move |body| {
                        let v: Value = serde_json::from_slice(&body)
                            .map_err(|e| {
                                ::std::io::Error::new(std::io::ErrorKind::Other, e)
                            })?;
                        if stat == StatusCode::Ok {
                            assert_eq!(v["token_type"], Value::String("bearer".to_owned()));
                            Ok(v.get("access_token").unwrap().as_str().unwrap().to_owned())
                        } else {
                            assert!(v["errors"].as_array().unwrap().len() == 1);
                            bail!(::std::io::Error::new(std::io::ErrorKind::Other,
                                                        v["errors"][0]["message"]
                                                            .as_str()
                                                            .unwrap()
                                                            .to_owned()))
                        }
                    })
                    .and_then(move |token| {
                        Ok(Twitter {
                            client: client,
                            bearer: token,
                        })
                    })
            });

        core.run(work)
            .map_err(|e| e.into())
    }
}

#[test]
#[ignore]
fn test_auth() {
    let mut core = Core::new().unwrap();
    let twit = Twitter::authenticate_with(&mut core,
                                          ApplicationAuth {
                                              key: "".to_owned(),
                                              secret: "".to_owned(),
                                          })
        .unwrap();

    println!("{}", twit.bearer);
}

#[test]
#[should_panic]
fn test_auth_fail() {
    let mut core = Core::new().unwrap();
    Twitter::authenticate_with(&mut core,
                               ApplicationAuth {
                                   key: "foo".to_owned(),
                                   secret: "bar".to_owned(),
                               })
        .unwrap();
}
