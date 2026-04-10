use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;

use crate::span_builder::*;

/// Normal API → auth → db request, all healthy.
pub fn healthy_api_request() -> ExportTraceServiceRequest {
    let tid = trace_id();

    let root = SpanSpec::new("HTTP GET /users", &tid)
        .duration(45)
        .attr(str_attr("http.method", "GET"))
        .attr(str_attr("http.route", "/users"))
        .attr(int_attr("http.status_code", 200));
    let root_sid = root.span_id.clone();

    let auth = SpanSpec::new("auth.verify_token", &tid)
        .parent(&root_sid)
        .offset(2)
        .duration(8)
        .attr(str_attr("auth.method", "jwt"));
    let db = SpanSpec::new("SELECT * FROM users", &tid)
        .parent(&root_sid)
        .offset(12)
        .duration(25)
        .attr(str_attr("db.system", "postgresql"))
        .attr(str_attr("db.statement", "SELECT * FROM users WHERE active = true"));

    ExportTraceServiceRequest {
        resource_spans: vec![
            resource_spans("api-gateway", vec![root.build()]),
            resource_spans("auth-service", vec![auth.build()]),
            resource_spans("user-service", vec![db.build()]),
        ],
    }
}

/// API request with a slow DB query (> 500ms).
pub fn slow_db_query() -> ExportTraceServiceRequest {
    let tid = trace_id();

    let root = SpanSpec::new("HTTP GET /orders", &tid)
        .duration(820)
        .attr(str_attr("http.method", "GET"))
        .attr(str_attr("http.route", "/orders"))
        .attr(int_attr("http.status_code", 200));
    let root_sid = root.span_id.clone();

    let db = SpanSpec::new("SELECT orders JOIN products", &tid)
        .parent(&root_sid)
        .offset(5)
        .duration(780)
        .attr(str_attr("db.system", "postgresql"))
        .attr(str_attr("db.statement", "SELECT o.*, p.name FROM orders o JOIN products p ON o.product_id = p.id WHERE o.user_id = $1"))
        .event("slow_query_warning", vec![
            str_attr("message", "query exceeded 500ms threshold"),
        ]);

    ExportTraceServiceRequest {
        resource_spans: vec![
            resource_spans("api-gateway", vec![root.build()]),
            resource_spans("order-service", vec![db.build()]),
        ],
    }
}

/// Payment processing that fails with an error.
pub fn payment_error() -> ExportTraceServiceRequest {
    let tid = trace_id();

    let root = SpanSpec::new("HTTP POST /checkout", &tid)
        .duration(340)
        .error()
        .attr(str_attr("http.method", "POST"))
        .attr(str_attr("http.route", "/checkout"))
        .attr(int_attr("http.status_code", 500));
    let root_sid = root.span_id.clone();

    let validate = SpanSpec::new("cart.validate", &tid)
        .parent(&root_sid)
        .offset(3)
        .duration(15);

    let payment = SpanSpec::new("payment.charge", &tid)
        .parent(&root_sid)
        .offset(20)
        .duration(310)
        .error()
        .attr(str_attr("payment.provider", "stripe"))
        .attr(str_attr("error.type", "PaymentDeclined"))
        .event("exception", vec![
            str_attr("exception.type", "PaymentDeclinedException"),
            str_attr("exception.message", "Card declined: insufficient funds"),
        ]);

    ExportTraceServiceRequest {
        resource_spans: vec![
            resource_spans("api-gateway", vec![root.build()]),
            resource_spans("cart-service", vec![validate.build()]),
            resource_spans("payment-service", vec![payment.build()]),
        ],
    }
}

/// API gateway fans out to 3 downstream services in parallel.
pub fn fanout_request() -> ExportTraceServiceRequest {
    let tid = trace_id();

    let root = SpanSpec::new("HTTP GET /dashboard", &tid)
        .duration(200)
        .attr(str_attr("http.method", "GET"))
        .attr(str_attr("http.route", "/dashboard"))
        .attr(int_attr("http.status_code", 200));
    let root_sid = root.span_id.clone();

    let user_fetch = SpanSpec::new("fetch_user_profile", &tid)
        .parent(&root_sid)
        .offset(5)
        .duration(60)
        .attr(str_attr("downstream.service", "user-service"));

    let orders_fetch = SpanSpec::new("fetch_recent_orders", &tid)
        .parent(&root_sid)
        .offset(5)
        .duration(120)
        .attr(str_attr("downstream.service", "order-service"));

    let notif_fetch = SpanSpec::new("fetch_notifications", &tid)
        .parent(&root_sid)
        .offset(5)
        .duration(45)
        .attr(str_attr("downstream.service", "notification-service"));

    ExportTraceServiceRequest {
        resource_spans: vec![
            resource_spans("api-gateway", vec![root.build()]),
            resource_spans("user-service", vec![user_fetch.build()]),
            resource_spans("order-service", vec![orders_fetch.build()]),
            resource_spans("notification-service", vec![notif_fetch.build()]),
        ],
    }
}

/// Burst of 10 fast, healthy requests to simulate normal traffic.
pub fn fast_burst() -> ExportTraceServiceRequest {
    let mut all_spans = Vec::new();

    for i in 0..10 {
        let tid = trace_id();
        let routes = ["/health", "/api/v1/status", "/users/me", "/config", "/ping"];
        let route = routes[i % routes.len()];

        let span = SpanSpec::new(&format!("HTTP GET {route}"), &tid)
            .duration(2 + (i as u64 * 3))
            .offset(i as u64 * 50)
            .attr(str_attr("http.method", "GET"))
            .attr(str_attr("http.route", route))
            .attr(int_attr("http.status_code", 200));

        all_spans.push(span.build());
    }

    ExportTraceServiceRequest {
        resource_spans: vec![resource_spans("api-gateway", all_spans)],
    }
}
