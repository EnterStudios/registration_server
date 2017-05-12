// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use config::Config;
use database::{DatabaseError, DomainRecord};
use errors::*;
use iron::headers::ContentType;
use iron::prelude::*;
use iron::status::{self, Status};
use params::{FromValue, Params, Value};
use pdns::pdns;
use router::Router;
use serde_json;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

fn domain_for_name(name: &str, config: &Config) -> String {
    format!("{}.box.{}.", name, config.domain).to_lowercase()
}

#[derive(Serialize)]
struct Discovered {
    href: String,
    desc: String,
}

fn register(req: &mut Request, config: &Config) -> IronResult<Response> {
    // Extract the local_ip and token parameter,
    // and the public IP from the socket.
    let public_ip = format!("{}", req.remote_addr.ip());

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let token = map.find(&["token"]);
    let local_ip = map.find(&["local_ip"]);

    // Both parameters are mandatory.
    if token.is_none() || local_ip.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }

    let token = String::from_value(token.unwrap()).unwrap();
    let local_ip = String::from_value(local_ip.unwrap()).unwrap();

    info!("GET /register token={} local_ip={} public_ip={}",
          token,
          local_ip,
          public_ip);

    // Save this registration in the database if we know about this token.
    // Check if we have a record with this token, bail out if not.
    match config.db.get_record_by_token(&token).recv().unwrap() {
        Ok(record) => {
            // Update the record with the challenge.
            let dns_challenge = match record.dns_challenge {
                Some(ref challenge) => Some(challenge.as_str()),
                None => None,
            };
            // Update the timestamp to be current.
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let email = match record.email {
                Some(ref email) => Some(email.as_str()),
                None => None,
            };
            let new_record = DomainRecord::new(&record.token,
                                               &record.local_name,
                                               &record.remote_name,
                                               dns_challenge,
                                               Some(&local_ip),
                                               Some(&public_ip),
                                               &record.description,
                                               email,
                                               timestamp);
            match config.db.update_record(new_record).recv().unwrap() {
                Ok(()) => {
                    // Everything went fine, return an empty 200 OK for now.
                    let mut response = Response::new();
                    response.status = Some(Status::Ok);

                    Ok(response)
                }
                Err(_) => EndpointError::with(status::InternalServerError, 501),
            }
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn info(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /info");

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let token = map.find(&["token"]);
    if token.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }
    let token = String::from_value(token.unwrap()).unwrap();

    match config.db.get_record_by_token(&token).recv().unwrap() {
        Ok(record) => {
            let mut response = Response::with(serde_json::to_string(&record).unwrap());
            response.headers.set(ContentType::json());
            Ok(response)
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

// Public ping endpoint, returning names of servers on the same
// local network than the client.
fn ping(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /ping");

    let remote_ip = format!("{}", req.remote_addr.ip());

    match config
              .db
              .get_records_by_public_ip(&remote_ip)
              .recv()
              .unwrap() {
        Ok(records) => {
            let results: Vec<Discovered> = records
                .into_iter()
                .map(|item| {
                         Discovered {
                             href: format!("https://{}",
                                           item.local_name[..item.local_name.len() - 1].to_owned()),
                             desc: item.description,
                         }
                     })
                .collect();

            let mut response = Response::with(serde_json::to_string(&results).unwrap());
            response.headers.set(ContentType::json());
            Ok(response)
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn unsubscribe(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /unsubscribe");

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let token = map.find(&["token"]);
    if token.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }
    let token = String::from_value(token.unwrap()).unwrap();

    match config
              .db
              .delete_record_by_token(&token)
              .recv()
              .unwrap() {
        Ok(0) => EndpointError::with(status::BadRequest, 400), // No record found for this token.
        Ok(_) => {
            let mut response = Response::new();
            response.status = Some(Status::Ok);

            Ok(response)
        }
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn subscribe(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /subscribe");

    // Extract the name parameter.
    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    match map.find(&["name"]) {
        Some(&Value::String(ref name)) => {
            let full_name = domain_for_name(name, config);
            info!("trying to subscribe {}", full_name);

            let record = config
                .db
                .get_record_by_name(&full_name)
                .recv()
                .unwrap();
            match record {
                Ok(_) => {
                    // We already have a record for this name, return an error.
                    let mut response = Response::with("{\"error\": \"UnavailableName\"}");
                    response.status = Some(Status::BadRequest);
                    response.headers.set(ContentType::json());
                    Ok(response)
                }
                Err(DatabaseError::NoRecord) => {
                    // Create a token, create and store a record and finally return the token.
                    let token = format!("{}", Uuid::new_v4());
                    let local_name = format!("local.{}", full_name);


                    let description = match map.find(&["desc"]) {
                        Some(&Value::String(ref desc)) => desc.to_owned(),
                        _ => format!("{}'s server", name),
                    };
                    let record = DomainRecord::new(&token,
                                                   &local_name,
                                                   &full_name,
                                                   None,
                                                   None,
                                                   None,
                                                   &description,
                                                   None,
                                                   0);
                    match config.db.add_record(record).recv().unwrap() {
                        Ok(()) => {
                            // We don't want the full domain name or the dns challenge in the
                            // response so we create a local struct.
                            #[derive(Serialize)]
                            struct NameAndToken {
                                name: String,
                                token: String,
                            }
                            let n_and_t = NameAndToken {
                                name: name.to_owned(),
                                token: token,
                            };
                            match serde_json::to_string(&n_and_t) {
                                Ok(serialized) => {
                                    let mut response = Response::with(serialized);
                                    response.status = Some(Status::Ok);
                                    response.headers.set(ContentType::json());

                                    Ok(response)
                                }
                                Err(_) => EndpointError::with(status::InternalServerError, 501)
                            }
                        }
                        Err(_) => EndpointError::with(status::InternalServerError, 501),
                    }
                }
                // Other error, like a db issue.
                Err(_) => EndpointError::with(status::InternalServerError, 501),
            }
        }
        // Missing `name` parameter.
        _ => EndpointError::with(status::BadRequest, 400),
    }
}

fn dnsconfig(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /dnsconfig");

    // Extract the challenge and token parameter.
    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let challenge = map.find(&["challenge"]);
    let token = map.find(&["token"]);

    // Both parameters are mandatory.
    if challenge.is_none() || token.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }

    let challenge = String::from_value(challenge.unwrap()).unwrap();
    let token = String::from_value(token.unwrap()).unwrap();

    // Check if we have a record with this token, bail out if not.
    match config.db.get_record_by_token(&token).recv().unwrap() {
        Ok(record) => {
            // Update the record with the challenge.
            let local_ip = match record.local_ip {
                Some(ref ip) => Some(ip.as_str()),
                None => None,
            };
            let public_ip = match record.public_ip {
                Some(ref ip) => Some(ip.as_str()),
                None => None,
            };
            let email = match record.email {
                Some(ref email) => Some(email.as_str()),
                None => None,
            };
            let new_record = DomainRecord::new(&record.token,
                                               &record.local_name,
                                               &record.remote_name,
                                               Some(&challenge),
                                               local_ip,
                                               public_ip,
                                               &record.description,
                                               email,
                                               record.timestamp);
            match config.db.update_record(new_record).recv().unwrap() {
                Ok(()) => {
                    // Everything went fine, return an empty 200 OK for now.
                    let mut response = Response::new();
                    response.status = Some(Status::Ok);

                    Ok(response)
                }
                Err(_) => EndpointError::with(status::InternalServerError, 501),
            }
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn adddiscovery(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /adddiscovery");

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let token = map.find(&["token"]);
    let disco = map.find(&["disco"]);

    if token.is_none() || disco.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }

    let token = String::from_value(token.unwrap()).unwrap();
    let disco = String::from_value(disco.unwrap()).unwrap();

    match config.db.get_record_by_token(&token).recv().unwrap() {
        Ok(_) => {
            match config.db.add_discovery(&token, &disco).recv().unwrap() {
                Ok(()) => {
                    let mut response = Response::new();
                    response.status = Some(Status::Ok);

                    Ok(response)
                }
                Err(_) => EndpointError::with(status::BadRequest, 400),
            }
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn revokediscovery(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /revokediscovery");

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let token = map.find(&["token"]);
    let disco = map.find(&["disco"]);

    if token.is_none() || disco.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }

    let token = String::from_value(token.unwrap()).unwrap();
    let disco = String::from_value(disco.unwrap()).unwrap();

    match config.db.get_record_by_token(&token).recv().unwrap() {
        Ok(_) => {
            match config.db.delete_discovery(&disco).recv().unwrap() {
                Ok(_) => {
                    let mut response = Response::new();
                    response.status = Some(Status::Ok);

                    Ok(response)
                }
                Err(_) => EndpointError::with(status::BadRequest, 400),
            }
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

fn discovery(req: &mut Request, config: &Config) -> IronResult<Response> {
    info!("GET /discovery");

    let remote_ip = format!("{}", req.remote_addr.ip());

    let map = req.get_ref::<Params>().unwrap(); // TODO: don't unwrap.
    let disco = map.find(&["disco"]);

    if disco.is_none() {
        return EndpointError::with(status::BadRequest, 400);
    }

    let disco = String::from_value(disco.unwrap()).unwrap();

    match config
              .db
              .get_token_for_discovery(&disco)
              .recv()
              .unwrap() {
        Ok(token) => {
            match config
                      .db
                      .get_records_by_public_ip(&remote_ip)
                      .recv()
                      .unwrap() {
                Ok(records) => {
                    // Filter out and only return the record that matches the token.
                    let results: Vec<Discovered> = records
                        .into_iter()
                        .filter(|item| item.token == token)
                        .map(|item| {
                                 Discovered {
                                     href: format!("https://{}",
                                                   item.local_name[..item.local_name.len() - 1]
                                                       .to_owned()),
                                     desc: item.description,
                                 }
                             })
                        .collect();

                    if results.is_empty() {
                        // If the result vector is empty, return the remote name for this token.
                        match config.db.get_record_by_token(&token).recv().unwrap() {
                            Ok(record) => {
                                let len = record.remote_name.len() - 1;
                                let result = vec![Discovered {
                                                      href: format!("https://{}",
                                                                    record.remote_name[..len]
                                                                        .to_owned()),
                                                      desc: record.description,
                                                  }];
                                let mut response = Response::with(serde_json::to_string(&result)
                                                                      .unwrap());
                                response.headers.set(ContentType::json());
                                Ok(response)
                            }
                            Err(_) => EndpointError::with(status::BadRequest, 400),
                        }
                    } else {
                        let mut response = Response::with(serde_json::to_string(&results).unwrap());
                        response.headers.set(ContentType::json());
                        Ok(response)
                    }
                }
                Err(_) => EndpointError::with(status::BadRequest, 400),
            }
        }
        Err(DatabaseError::NoRecord) => EndpointError::with(status::BadRequest, 400),
        Err(_) => EndpointError::with(status::InternalServerError, 501),
    }
}

pub fn create(config: &Config) -> Router {
    let mut router = Router::new();

    macro_rules! handler {
        ($name:ident) => (
            let config_ = config.clone();
            router.get(stringify!($name),
                       move |req: &mut Request| -> IronResult<Response> {
                $name(req, &config_)
            }, stringify!($name));
        )
    }

    handler!(register);
    handler!(info);
    handler!(subscribe);
    handler!(unsubscribe);
    handler!(dnsconfig);
    handler!(ping);
    handler!(adddiscovery);
    handler!(revokediscovery);
    handler!(discovery);

    if config.socket_path.is_none() {
        handler!(pdns);
    }

    router
}
