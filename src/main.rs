#![feature(proc_macro_hygiene)]

extern crate hyper;
extern crate maud;
extern crate futures;
extern crate url;

#[macro_use]
extern crate serde_json;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate diesel;

#[macro_use]
extern crate log;
extern crate env_logger;

use std::collections::HashMap;
use std::error::Error;
use std::env;
use std::io;

use hyper::{Chunk, StatusCode};
use hyper::Method::{Get, Post};
use hyper::server::{Request, Response, Service};
use hyper::header::{ContentLength, ContentType};

use futures::Stream;
use futures::future::{Future, FutureResult};

use diesel::prelude::*;
use diesel::pg::PgConnection;

use maud::html;

mod models;
mod schema;

use models::{Message, NewMessage};

const DEFAULT_DATABASE_URL: &str = env::var("DATABASE_URL").expect("DATABASE_URL is not set!");  // Reads DATABASE_URL value from .env file

struct Microservice;

struct TimeRange {
    before: Option<i64>,
    after: Option<i64>,
}

fn parse_form(form_chunk: Chunk) -> FutureResult<NewMessage, hyper::Error> {
    /// Receives a Chunk (a message body), and parses out the username and message while handling errors appropriately
    let mut form = url::form_urlencoded::parse(form_chunk.as_ref())  // Parse the form
        .into_owned()
        .collect::<HashMap<String, String>>();  // Parse the form into a HashMap

    if let Some(message) = form.remove("message") {  // Attempt to remove the message key from it
        let username = form.remove("username").unwrap_or(String::from("anonymous"));  // Default username to "ananymous" if it's not there 
        futures::future::ok(NewMessage { username, message })  // Return future containing our simple `NewMessage` struct
    } else {  // If attempt fails, return an error since a message is mandatory
        futures::future::err(hyper::Error::from(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Missing field 'message",
        )))
    }
}

fn write_to_db(
    new_message: NewMessage,
    db_connection: &PgConnection,
) -> FutureResult<i64, hyper::Error> {
    use schema::messages;
    let timestamp = diesel::insert_into(messages::table)
        .values(&new_message)
        .returning(messages::timestamp)
        .get_result(db_connection);

    match timestamp {
        Ok(timestamp) => futures::future::ok(timestamp),
        Err(error) => {
            error!("Error writing to database: {}", error.description());
            futures::future::err(hyper::Error::from(
                io::Error::new(io::ErrorKind::Other, "service error"),
            ))
        }
    }
}


fn make_error_response(error_message: &str) -> FutureResult<hyper::Response, hyper::Error> {
    let payload = json!({
        "error": error_message
    }).to_string();
    // When constructing a response struct, we need to set correct HTTP headers
    let response = Response::new()
        .with_status(StatusCode::InternalServerError)  // Set the HTTP status of the response to InternalServiceError (status 500)
        .with_header(ContentLength(payload.len() as u64))  // Set the Content-Length header to the length of the response body
        .with_header(ContentType::json())  // Set the Content-Type header to application/json
        .with_body(payload);
    debug!("{:?}", response);
    futures::future::ok(response)
}

fn make_post_response(
    result: Result<i64, hyper::Error>,
) -> FutureResult<hyper::Response, hyper::Error> {
    /// Return a response back to whoever blessed our microservice with a request
    match result {  // Match on the `result` to see if we were able to write to database
        Ok(timestamp) => {
            // Create a JSON payload forming the body of the response we return
            let payload = json!({
                "timestamp": timestamp
            }).to_string();
            // When constructing a response struct, we need to set correct HTTP headers
            let response = Response::new()
                // .with_header(StatusCode::Ok)  // Default status is OK(200), therefore we don't need to set it 
                .with_header(ContentLength(payload.len() as u64))  // Set the Content-Length header to the length of the response body
                .with_header(ContentType::json())  // Set the Content-Type header to application/json
                .with_body(payload);
            debug!("{:?}", response);
            futures::future::ok(response)
        }
        // Refactored out the code to make a response struct for erroneous case
        Err(error) => make_error_response(error.description()),
    }
}

fn parse_query(query: &str) -> Result<TimeRange, String> {
    // Parse the form into a hashmap, since the syntax is still `key=value&key=value`
    let args = url::form_urlencoded::parse(&query.as_bytes())
        .into_owned()
        .collect::<HashMap<String, String>>();

    // Try to get `before` field from the form
    // If there, parse to i64
    let before = args.get("before").map(|value| value.parse::<i64>());
    if let Some(Err(ref error)) = before {
        return Err(format!("Error parsing 'before': {}", error));
    }

    // Try to get `after` field from the form
    // If there, parse to i64
    let after = args.get("after").map(|value| value.parse::<i64>());
    if let Some(Err(ref error)) = after {
        return Err(format!("Error parsing 'after': {}", error));
    }
    
    Ok(TimeRange {
        before: before.map(|b| b.unwrap()),
        after: after.map(|a| a.unwrap()),
    })
}

fn query_db(time_range: TimeRange, db_connection: &PgConnection) -> Option<Vec<Message>> {
    use schema::messages;
    let TimeRange { before, after } = time_range;

    let mut query = messages::table.into_boxed();

    if let Some(before) = before {
        query = query.filter(messages::timestamp.lt(before as i64))
    }

    if let Some(after) = after {
        query = query.filter(messages::timestamp.gt(after as i64))
    }

    let query_result = query.load::<Message>(db_connection);

    match query_result {
        Ok(result) => Some(result),
        Err(error) => {
            error!("Error querying DB: {}", error);
            None
        }
    }
}

fn render_page(messages: Vec<Message>) -> String {
    (html! {
        head {
            title {"microservice"}
            style {"body { font-family: monospace }"}
        }
        body {
            ul {
                @for message in &messages {
                    li {
                        (message.username) " (" (message.timestamp) "): " (message.message)
                    }
                }
            }
        }
    }).into_string()
}

fn make_get_response(
    messages: Option<Vec<Message>>,
) -> FutureResult<hyper::Response, hyper::Error> {
    let response = match messages {
        Some(messages) => {  // If the messages option contains a value
            let body = render_page(messages);  // Pass the messages on to render_page, which will return an HTML page that forms the body of our response,
            Response::new()
                .with_header(ContentLength(body.len() as u64))
                .with_header(ContentType::html())
                .with_body(body)
        }
        None => Response::new().with_status(StatusCode::InternalServerError),
    };
    debug!("{:?}", response);
    futures::future::ok(response)
}

fn connect_to_db() -> Option<PgConnection> {
    let database_url = env::var("DATABASE_URL").unwrap_or(String::from(DEFAULT_DATABASE_URL));
    match PgConnection::establish(&database_url) {
        Ok(connection) => Some(connection),
        Err(error) => {
            error!("Error connecting to database: {}", error.description());
            None
        }
    }
}

impl Service for Microservice {  // Basic types for our service
    type Request = Request;  // 
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;  // Future type is boxed because it is a trait

    fn call(&self, request: Request) -> Self::Future { . // hyper::Request is an object representing a parsed HTTP request
        debug!("{:?}", request);
        let db_connection = match connect_to_db() {
            Some(connection) => connection,
            None => {
                return Box::new(futures::future::ok(
                    Response::new().with_status(StatusCode::InternalServerError),
                ))
            }
        };
        // Distinguish between different requests by matching on the method and path of the request
        match (request.method(), request.path()) {
            // Accept POST requests to our service’s root path ("/") and expect them to contain a username and message field in their form data.
            (&Post, "/") => {
                let future = request
                    .body()
                    .concat2()
                    // `and_then` combinator will call a function with the value contained in a future
                    .and_then(parse_form)  // Returns a new future
                    // and then pass that information on to a function that writes the values of those fields into a database
                    .and_then(move |new_message| write_to_db(new_message, &db_connection))
                    // Executes its callback regardless of the future's state
                    .then(make_post_response);
                Box::new(future)  // Return a response
            }
            // Sent to our server to fetch messages
            (&Get, "/") => { 
                // Request is allowed to have two query arguments, `before` and `after`, both timestamps to constrain
                // the messages fetched according to their timestamp, and both are optional
                let time_range = match request.query() {  // `request.query()` returns an `Option<&str>, since a URI may not have a query string at all
                    // If a query string is present, call `parse_query`, which parses the arguments and returns a TimeRange struct
                    Some(query) => parse_query(query),
                    // If query string is not present, create a TimeRange with values as None
                    None => Ok(TimeRange {
                        before: None,
                        after: None,
                    }), 
                };
                let response = match time_range {
                    // Fetch the messages for us, and `make_get_response`, which creates an appropriate Response object to return back to the client
                    Ok(time_range) => make_get_response(query_db(time_range, &db_connection)),
                    // Timestamps may be invalid (e.g. not numeric), so we have to deal with the case where parsing their values fails
                    // In such a case, parse_query will return an error message, which we can forward to `make_error_response`
                    Err(error) => make_error_response(&error),  // 
                };
                Box::new(response)
            }
            _ => Box::new(futures::future::ok(
                Response::new().with_status(StatusCode::NotFound),
            )),
        }
    }
}

fn main() {
    env_logger::init();
    let address = "127.0.0.1:8080".parse().unwrap(); 
    // New instance is created for each new request
    let server = hyper::server::Http::new()  // Binding IP address to an Http instance
        .bind(&address, move || Ok(Microservice))
        .unwrap();
    info!("Running microservice at {}", address);
    server.run().unwrap();  // Start the server
}
