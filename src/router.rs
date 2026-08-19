// dispatch a parsed request: the /_fake admin plane first, then the matrix
// client-server api, else a 404.

use crate::admin;
use crate::matrix;
use crate::response::{HttpResponse, ParsedRequest};
use crate::state::Shared;

pub fn route(state: &Shared, req: &ParsedRequest) -> HttpResponse {
    let path = req.path.split('?').next().unwrap_or(&req.path).to_string();
    crate::log(format_args!("{} {}", req.method, req.path));
    if path.starts_with(admin::PREFIX) {
        return admin::handle(state, req, &path);
    }
    if path.starts_with(matrix::PREFIX) {
        return matrix::handle(state, req, &path);
    }
    crate::log(format_args!("404 {} {}", req.method, path));
    HttpResponse::not_found(&req.method, &path)
}
