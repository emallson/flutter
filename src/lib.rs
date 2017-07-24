#[macro_use]
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
extern crate url;

pub mod errors;
use errors::*;

use hyper::{Method, Client, Request, StatusCode};
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use serde_json::Value;
use futures::{Stream, Future, Poll, Async};
use serde::de::DeserializeOwned;
use tokio_core::reactor::Timeout;
use url::Url;
use std::time::{UNIX_EPOCH, Duration, SystemTime};

header! { (XRateLimitReset, "x-rate-limit-reset") => [usize] }

#[derive(Serialize, Deserialize)]
pub struct ApplicationAuth {
    key: String,
    secret: String,
}

pub struct Twitter {
    client: Client<HttpsConnector<HttpConnector>>,
    bearer: String,
}

pub type UserId = usize;

#[derive(Serialize, Deserialize)]
pub struct User {
    // todo
}

#[derive(Clone)]
struct CursorData<T> {
    values: Vec<T>,
    next_cursor_str: String,
    prev_cursor_str: String,
}

impl<T> Default for CursorData<T> {
    fn default() -> Self {
        CursorData {
            values: vec![],
            next_cursor_str: String::default(),
            prev_cursor_str: String::default(),
        }
    }
}

impl<T: DeserializeOwned> CursorData<T> {
    fn from_slice(slice: &[u8]) -> ::std::result::Result<CursorData<T>, serde_json::Error> {
        // there should *ideally* only be one data entry per cursor item.
        let map: Value = serde_json::from_slice(slice)?;
        let mut cursor_data = CursorData::default();
        for (k, v) in map.as_object().unwrap().into_iter() {
            if k == "next_cursor_str" {
                cursor_data.next_cursor_str = v.as_str().unwrap().to_owned();
            } else if k == "prev_cursor_str" {
                cursor_data.prev_cursor_str = v.as_str().unwrap().to_owned();
            } else {
                let vec: ::std::result::Result<Vec<T>, _> = serde_json::from_value(v.clone());
                if vec.is_ok() {
                    cursor_data.values = vec.unwrap();
                }
            }
        }
        Ok(cursor_data)
    }
}

pub struct Cursor<'a, T: DeserializeOwned> {
    previous_cursor_str: Option<String>,
    next_cursor_str: Option<String>,
    current_items: Vec<T>,
    client: &'a Twitter,
    url: Url,
    next_request: Option<Box<Future<Item = Option<CursorData<T>>, Error = Error> + 'a>>,
}

impl<'a, T: DeserializeOwned + 'a> Cursor<'a, T> {
    pub fn new(twit: &'a Twitter, base: Url) -> Self {
        Cursor {
            previous_cursor_str: None,
            next_cursor_str: None,
            current_items: vec![],
            client: twit,
            url: base,
            next_request: None,
        }
    }

    fn request_next_page(&mut self) {
        assert!(self.next_request.is_none());
        let mut url = self.url.clone();
        if self.next_cursor_str.is_some() {
            url.query_pairs_mut().append_pair("cursor", self.next_cursor_str.as_ref().unwrap());
        }
        let req = self.client.authenticated_request(Method::Get, url);

        let handle = self.client.client.handle().clone();
        let bx: Box<Future<Item = Option<CursorData<T>>, Error = Error>> = Box::new(self.client
            .client
            .request(req)
            .map_err(|err| -> Error { err.into() })
            .and_then(move |res| {
                if res.status() == StatusCode::TooManyRequests {
                    let &XRateLimitReset(secs) = res.headers().get::<XRateLimitReset>().unwrap();
                    let dt = UNIX_EPOCH + Duration::new(secs as u64, 0);
                    let now = SystemTime::now();
                    let b: Box<Future<Item = Option<CursorData<T>>, Error = Error>> =
                        Box::new(Timeout::new(dt.duration_since(now).unwrap(), &handle)
                            .unwrap()
                            .map(|_| None)
                            .map_err(|e| e.into()));
                    b
                } else {
                    let b: Box<Future<Item = Option<CursorData<T>>, Error = Error>> =
                        Box::new(res.body()
                            .concat2()
                            .map_err(|err| -> Error { err.into() })
                            .and_then(move |body| {
                                CursorData::from_slice(&body).map_err(|e| e.into())
                            })
                            .map(move |val| Some(val)));
                    b
                }
            }));

        self.next_request = Some(bx);
    }
}

impl<'a, T: DeserializeOwned + 'a> Stream for Cursor<'a, T> {
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if let Some(ref mut next_req) = self.next_request {
            match next_req.poll() {
                Ok(Async::Ready(Some(obj))) => {
                    self.previous_cursor_str = Some(obj.prev_cursor_str);
                    self.next_cursor_str = Some(obj.next_cursor_str);
                    self.current_items = obj.values;
                }
                Ok(Async::Ready(None)) => {
                    // do nothing
                }
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Err(e) => return Err(e),
            }
        }
        self.next_request = None; // either no request pending (so idempotent) or we're done with the request
        // no pending request
        if self.current_items.is_empty() {
            if let Some(ref s) = self.next_cursor_str {
                if s == "0" {
                    return Ok(Async::Ready(None));
                }
            }
            self.request_next_page();
            match self.next_request.as_mut().poll() {
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                _ => unreachable!(),
            }
        } else {
            return Ok(Async::Ready(self.current_items.pop()));
        }
    }
}

impl Twitter {
    /// Construct a `Twitter`.
    ///
    /// Note that this function *blocks* since returning the future to complete the request is
    /// actually an enormous pain.
    pub fn authenticate_with(handle: &::tokio_core::reactor::Handle,
                             auth: ApplicationAuth)
                             -> Result<Box<Future<Item = Self, Error = Error>>> {
        use hyper::header::{Authorization, Basic, ContentType};
        use hyper::mime::Mime;
        use std::str::FromStr;

        let client = Client::configure()
            .connector(HttpsConnector::new(4, handle).unwrap())
            .build(handle);

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
            .map_err(|err| -> Error { err.into() })
            .and_then(|res| {
                let stat = res.status();
                res.body()
                    .concat2()
                    .map_err(|err| -> Error { err.into() })
                    .and_then(move |body| {
                        let v: Value = serde_json::from_slice(&body)?;
                        if stat == StatusCode::Ok {
                            assert_eq!(v["token_type"], Value::String("bearer".to_owned()));
                            Ok(v.get("access_token").unwrap().as_str().unwrap().to_owned())
                        } else {
                            assert!(v["errors"].as_array().unwrap().len() == 1);
                            bail!(v["errors"][0]["message"]
                                .as_str()
                                .unwrap()
                                .to_owned())
                        }
                    })
                    .and_then(move |token| {
                        Ok(Twitter {
                            client: client,
                            bearer: token,
                        })
                    })
            });
        Ok(Box::new(work))
    }

    pub fn list_friend_ids(&self, user: UserId) -> Cursor<UserId> {
        let url = Url::parse_with_params("https://api.twitter.com/1.1/friends/ids.json",
                                         &[("user_id", user.to_string().as_str())])
            .unwrap();
        Cursor::new(self, url)
    }

    pub fn list_follower_ids(&self, user: UserId) -> Cursor<UserId> {
        let url = Url::parse_with_params("https://api.twitter.com/1.1/followers/ids.json",
                                         &[("user_id", user.to_string().as_str())])
            .unwrap();
        Cursor::new(self, url)
    }

    fn authenticated_request(&self, method: Method, url: Url) -> Request {
        use hyper::header::{Authorization, Bearer};
        let mut req = Request::new(method, url.as_str().parse().unwrap());
        req.headers_mut().set(Authorization(Bearer { token: self.bearer.clone() }));
        req
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tokio_core::reactor::Core;
    const KEY: &str = "";
    const SECRET: &str = "";

    #[test]
    fn test_auth() {
        let mut core = Core::new().unwrap();
        let twit = Twitter::authenticate_with(&core.handle(),
                                              ApplicationAuth {
                                                  key: KEY.to_owned(),
                                                  secret: SECRET.to_owned(),
                                              })
            .unwrap();

        let twit = core.run(twit).unwrap();
        println!("{}", twit.bearer);
    }

    #[test]
    #[should_panic]
    fn test_auth_fail() {
        let mut core = Core::new().unwrap();
        let twit = Twitter::authenticate_with(&core.handle(),
                                              ApplicationAuth {
                                                  key: "foo".to_owned(),
                                                  secret: "bar".to_owned(),
                                              })
            .unwrap();
        core.run(twit).unwrap();
    }

    #[test]
    fn test_list_friend_ids() {
        let mut core = Core::new().unwrap();
        let twit = Twitter::authenticate_with(&core.handle(),
                                              ApplicationAuth {
                                                  key: KEY.to_owned(),
                                                  secret: SECRET.to_owned(),
                                              })
            .unwrap();

        let twit = core.run(twit).unwrap();

        let ids: Vec<_> = core.run(twit.list_friend_ids(17076641).collect()).unwrap();
        assert!(ids.len() > 0);
    }

    #[test]
    fn test_list_followers_ids() {
        let mut core = Core::new().unwrap();
        let twit = Twitter::authenticate_with(&core.handle(),
                                              ApplicationAuth {
                                                  key: KEY.to_owned(),
                                                  secret: SECRET.to_owned(),
                                              })
            .unwrap();

        let twit = core.run(twit).unwrap();

        let ids: Vec<_> = core.run(twit.list_follower_ids(17076641).collect()).unwrap();
        assert!(ids.len() > 0);
    }
}
