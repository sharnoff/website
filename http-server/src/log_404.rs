//! Wrapper module for the [`Log404`] fairing

use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Status;
use rocket::{Request, Response};

pub struct Log404;

impl Fairing for Log404 {
    fn info(&self) -> Info {
        Info {
            name: "Log 404",
            kind: Kind::Response,
        }
    }

    fn on_response(&self, request: &Request, response: &mut Response) {
        if response.status() != Status::NotFound {
            return;
        }

        let headers = request.headers();

        let ip = headers
            .get_one("X-Forwarded-For") // Set by caddy
            .map(str::to_owned)
            .or_else(|| Some(headers.get_one("X-Client-IP")?.to_string())) // Set by other proxies
            .or_else(|| Some(request.client_ip()?.to_string()));

        let referer = request.headers().get_one("Referer");
        let uri = request.uri();

        let yellow = "\x1b[33m";
        let reset = "\x1b[0m";

        match (referer, ip) {
            (None, None) => eprintln!("{yellow}404:{reset} {uri}"),
            (Some(r), None) => eprintln!("{yellow}404:{reset} [{r} =>]  {uri}"),
            (None, Some(ip)) => eprintln!("{yellow}404:{reset} {uri}  (by {ip})"),
            (Some(r), Some(ip)) => eprintln!("{yellow}404:{reset} [{r} =>]  {uri}  (by {ip})"),
        }
    }
}
