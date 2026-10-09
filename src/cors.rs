use actix_web::{
    HttpResponse,
    body::{BoxBody, MessageBody},
    dev::{ServiceRequest, ServiceResponse},
    http::{Method, header},
    middleware::Next,
};

const ALLOWED_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE";
const ALLOWED_HEADERS: &str =
    "Accept, Authorization, Content-Type, OpenTubeX-Client-Version, X-Pairing-Token";

/// Cross-origin clients supply explicit tokens. This policy does not approve
/// access using browser-managed cookies or HTTP authentication credentials.
pub async fn cors_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody + 'static>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    // The policy is constant across origins, methods, and requested headers;
    // neither origin reflection nor a Vary header is needed.
    if req.method() == Method::OPTIONS && req.headers().contains_key(header::ORIGIN) {
        let valid_method = req
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|method| ALLOWED_METHODS.split(", ").any(|allowed| allowed == method));
        let valid_headers = req
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .map(|value| {
                value.to_str().is_ok_and(|headers| {
                    headers.split(',').all(|name| {
                        ALLOWED_HEADERS
                            .split(", ")
                            .any(|allowed| allowed.eq_ignore_ascii_case(name.trim()))
                    })
                })
            })
            .unwrap_or(true);
        let response = if valid_method && valid_headers {
            HttpResponse::NoContent()
                .insert_header((header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"))
                .insert_header((header::ACCESS_CONTROL_ALLOW_METHODS, ALLOWED_METHODS))
                .insert_header((header::ACCESS_CONTROL_ALLOW_HEADERS, ALLOWED_HEADERS))
                .insert_header((header::ACCESS_CONTROL_MAX_AGE, "3600"))
                .finish()
        } else {
            HttpResponse::BadRequest().finish()
        };
        return Ok(req.into_response(response));
    }

    let mut response = next.call(req).await?.map_into_boxed_body();
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("Allow, Retry-After"),
    );
    Ok(response)
}
