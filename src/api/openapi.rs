//! OpenAPI 3.0 description of the REST API, generated from a compact table
//! so it stays next to the router it documents.

use axum::Json;
use serde_json::{json, Map, Value};

struct Op {
    method: &'static str,
    path: &'static str,
    tag: &'static str,
    summary: &'static str,
    /// Query parameters as `name:description`.
    query: &'static [&'static str],
    /// Example request body (JSON) for operations that take one.
    body: Option<&'static str>,
    admin: bool,
}

const fn op(
    method: &'static str,
    path: &'static str,
    tag: &'static str,
    summary: &'static str,
) -> Op {
    Op {
        method,
        path,
        tag,
        summary,
        query: &[],
        body: None,
        admin: false,
    }
}

const PAGE: &[&str] = &["limit:Page size (1-500, default 50)", "offset:Rows to skip"];
const MODEM: &[&str] = &["modem:Modem id (default: first modem)"];
const FILTERS: &[&str] = &[
    "direction:inbound or outbound",
    "type:Message type (sms, flash, binary, service, mms_notification, wap_push, unknown)",
    "status:Message status",
    "sender:Sender contains",
    "recipient:Recipient contains",
    "modem:Modem id",
    "search:Text contained in body, sender or recipient",
    "from:Created at or after (RFC 3339 or YYYY-MM-DD)",
    "to:Created at or before (RFC 3339 or YYYY-MM-DD)",
    "order:asc or desc (default desc)",
    "limit:Page size",
    "offset:Rows to skip",
];
const SEND: &str = r#"{"to": "+4500000000", "message": "Hello from THUGSred SMS Gateway", "encoding": "auto", "delivery_report": true, "priority": "normal"}"#;

fn ops() -> Vec<Op> {
    vec![
        op("get", "/health", "Health", "Liveness probe"),
        op(
            "get",
            "/ready",
            "Health",
            "Readiness probe (database reachable)",
        ),
        op("get", "/metrics", "Health", "Prometheus metrics"),
        op(
            "get",
            "/api/v1/status",
            "Status",
            "Daemon, modem, database, queue and integration status",
        ),
        op(
            "get",
            "/api/v1/version",
            "Status",
            "Version and build information",
        ),
        op(
            "get",
            "/api/v1/capabilities",
            "Status",
            "Gateway features and detected modem capabilities",
        ),
        op(
            "get",
            "/api/v1/stats",
            "Status",
            "Message, queue and integration statistics",
        ),
        Op {
            query: &["range:24h, 7d or 30d"],
            ..op(
                "get",
                "/api/v1/stats/activity",
                "Status",
                "Message activity per hour or day",
            )
        },
        op("get", "/api/v1/modems", "Modem", "All configured modems"),
        Op {
            query: MODEM,
            ..op(
                "get",
                "/api/v1/modem",
                "Modem",
                "Modem status and identification",
            )
        },
        Op {
            query: MODEM,
            ..op("get", "/api/v1/modem/signal", "Modem", "Signal strength")
        },
        Op {
            query: MODEM,
            ..op(
                "get",
                "/api/v1/modem/network",
                "Modem",
                "Network registration and operator",
            )
        },
        Op {
            query: MODEM,
            ..op(
                "get",
                "/api/v1/modem/storage",
                "Modem",
                "SMS storage usage on the modem/SIM",
            )
        },
        Op {
            query: MODEM,
            ..op(
                "get",
                "/api/v1/modem/health",
                "Modem",
                "Recent health samples",
            )
        },
        Op {
            query: MODEM,
            admin: true,
            ..op(
                "post",
                "/api/v1/modem/reconnect",
                "Modem",
                "Drop and re-establish the modem connection",
            )
        },
        Op {
            body: Some(r#"{"command": "AT+CSQ"}"#),
            admin: true,
            ..op(
                "post",
                "/api/v1/modem/at",
                "Modem",
                "Run a raw AT command (requires security.allow_raw_at_commands)",
            )
        },
        Op {
            query: MODEM,
            admin: true,
            ..op(
                "post",
                "/api/v1/modem/reset-pin-counter",
                "Modem",
                "Allow one new SIM PIN attempt after correcting sim.conf",
            )
        },
        Op {
            query: MODEM,
            ..op(
                "get",
                "/api/v1/sim",
                "SIM",
                "SIM state, detected identity and configured metadata",
            )
        },
        op(
            "get",
            "/api/v1/sims",
            "SIM",
            "SIM information for every modem",
        ),
        Op {
            body: Some(SEND),
            ..op(
                "post",
                "/api/v1/sms/send",
                "SMS",
                "Queue an SMS (202 Accepted). Supports the Idempotency-Key header",
            )
        },
        Op {
            body: Some(
                r#"{"to": "+4500000000", "message": "Reminder", "scheduled_at": "2030-01-01T09:00:00Z"}"#,
            ),
            ..op(
                "post",
                "/api/v1/sms/schedule",
                "SMS",
                "Queue an SMS for later transmission",
            )
        },
        Op {
            body: Some(r#"{"message": "How many parts is this?"}"#),
            ..op(
                "post",
                "/api/v1/sms/estimate",
                "SMS",
                "Encoding and segment estimate without sending",
            )
        },
        Op {
            body: Some(r#"{"pdu": "0011000B916407281553F80000AA0AE8329BFD4697D9EC37"}"#),
            admin: true,
            ..op(
                "post",
                "/api/v1/sms/send-pdu",
                "SMS",
                "Submit a raw SMS-SUBMIT PDU (requires security.allow_raw_pdu_submission)",
            )
        },
        Op {
            query: FILTERS,
            ..op(
                "get",
                "/api/v1/messages",
                "Messages",
                "Search message history",
            )
        },
        op(
            "get",
            "/api/v1/messages/{id}",
            "Messages",
            "Message details with decoded PDU and delivery reports",
        ),
        op(
            "get",
            "/api/v1/messages/{id}/parts",
            "Messages",
            "Message segments",
        ),
        op(
            "get",
            "/api/v1/messages/{id}/status",
            "Messages",
            "Delivery state and status history",
        ),
        Op {
            admin: true,
            ..op(
                "delete",
                "/api/v1/messages/{id}",
                "Messages",
                "Delete a message",
            )
        },
        Op {
            query: &[
                "status:Queue state, or all",
                "limit:Page size",
                "offset:Rows to skip",
            ],
            ..op("get", "/api/v1/queue", "Queue", "Outbound queue")
        },
        op(
            "get",
            "/api/v1/queue/{id}",
            "Queue",
            "Queue entry and its parts",
        ),
        Op {
            admin: true,
            ..op(
                "post",
                "/api/v1/queue/{id}/retry",
                "Queue",
                "Requeue a failed, expired, cancelled or uncertain message",
            )
        },
        op(
            "post",
            "/api/v1/queue/{id}/cancel",
            "Queue",
            "Cancel a message before transmission",
        ),
        Op {
            body: Some(r#"{"to": "+4500000000", "message": "hellohello"}"#),
            ..op(
                "post",
                "/api/v1/pdu/encode",
                "PDU",
                "Encode SMS-SUBMIT PDUs offline (nothing is transmitted)",
            )
        },
        Op {
            body: Some(
                r#"{"pdu": "07917283010010F5040BC87238880900F10000993092516195800AE8329BFD4697D9EC37"}"#,
            ),
            ..op(
                "post",
                "/api/v1/pdu/decode",
                "PDU",
                "Decode a PDU into structured fields",
            )
        },
        Op {
            query: PAGE,
            ..op("get", "/api/v1/calls", "Calls", "Call log")
        },
        op("get", "/api/v1/calls/{id}", "Calls", "Call details"),
        Op {
            query: PAGE,
            ..op("get", "/api/v1/recordings", "Calls", "Call recordings")
        },
        op(
            "get",
            "/api/v1/recordings/{id}/download",
            "Calls",
            "Download a recording (audio/wav)",
        ),
        op(
            "get",
            "/api/v1/integrations",
            "Integrations",
            "Configured integrations, routing rules and delivery statistics",
        ),
        op(
            "get",
            "/api/v1/integrations/{name}/status",
            "Integrations",
            "Integration health and recent deliveries",
        ),
        Op {
            admin: true,
            ..op(
                "post",
                "/api/v1/integrations/{name}/test",
                "Integrations",
                "Send a synthetic test event",
            )
        },
        Op {
            query: &[
                "integration:Integration name",
                "status:pending, retrying, delivered or dead",
                "limit:Page size",
                "offset:Rows to skip",
            ],
            ..op(
                "get",
                "/api/v1/deliveries",
                "Integrations",
                "Integration delivery queue and history",
            )
        },
        Op {
            admin: true,
            ..op(
                "post",
                "/api/v1/deliveries/{id}/replay",
                "Integrations",
                "Replay a dead or retrying delivery",
            )
        },
        Op {
            query: PAGE,
            ..op(
                "get",
                "/api/v1/dead-letters",
                "Integrations",
                "Dead-letter queue",
            )
        },
        Op {
            query: &[
                "type:Event type prefix, e.g. sms.",
                "since:Events at or after this time",
                "limit:Page size",
                "offset:Rows to skip",
            ],
            ..op("get", "/api/v1/events", "Events", "Stored gateway events")
        },
        Op {
            query: &["type:Event type prefix"],
            ..op(
                "get",
                "/api/v1/events/stream",
                "Events",
                "Live events (text/event-stream)",
            )
        },
        Op {
            query: &[
                "format:json or csv",
                "direction:inbound or outbound",
                "search:Text filter",
                "from:Start",
                "to:End",
                "limit:Maximum rows (default 10000)",
            ],
            ..op(
                "get",
                "/api/v1/history/export",
                "Messages",
                "Export message history",
            )
        },
        Op {
            query: PAGE,
            ..op("get", "/api/v1/ussd", "USSD", "USSD session history")
        },
        Op {
            body: Some(r#"{"command": "balance"}"#),
            ..op(
                "post",
                "/api/v1/ussd",
                "USSD",
                "Run a configured USSD command",
            )
        },
        op(
            "get",
            "/api/v1/gnss",
            "GNSS",
            "GNSS status and last position",
        ),
        op(
            "get",
            "/api/v1/addressbook",
            "Status",
            "Address book entries",
        ),
        Op {
            admin: true,
            ..op(
                "get",
                "/api/v1/config",
                "Administration",
                "Effective configuration with secrets redacted",
            )
        },
        op(
            "get",
            "/api/v1/config/status",
            "Administration",
            "Configuration validation and reload status",
        ),
        Op {
            admin: true,
            ..op(
                "post",
                "/api/v1/config/reload",
                "Administration",
                "Reload configuration from disk",
            )
        },
        Op {
            query: PAGE,
            admin: true,
            ..op("get", "/api/v1/audit", "Administration", "Audit trail")
        },
        Op {
            admin: true,
            ..op(
                "get",
                "/api/v1/diagnostics",
                "Administration",
                "Redacted diagnostic report",
            )
        },
        Op {
            admin: true,
            ..op(
                "post",
                "/api/v1/database/backup",
                "Administration",
                "Create and verify a database backup (SQLite)",
            )
        },
    ]
}

/// Build the OpenAPI document.
pub fn document() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for o in ops() {
        let mut params: Vec<Value> = o
            .query
            .iter()
            .map(|q| {
                let (name, desc) = q.split_once(':').unwrap_or((q, ""));
                json!({"name": name, "in": "query", "required": false, "description": desc, "schema": {"type": "string"}})
            })
            .collect();
        for seg in o.path.split('/').filter(|s| s.starts_with('{')) {
            params.push(json!({"name": seg.trim_matches(['{', '}']), "in": "path", "required": true, "schema": {"type": "string"}}));
        }
        if o.path == "/api/v1/sms/send" || o.path == "/api/v1/sms/schedule" {
            params.push(json!({"name": "Idempotency-Key", "in": "header", "required": false, "description": "Repeating a request with the same key returns the original message instead of sending again", "schema": {"type": "string", "maxLength": 128}}));
        }
        let mut operation = json!({
            "tags": [o.tag],
            "summary": o.summary,
            "operationId": format!("{}{}", o.method, o.path.replace(['/', '{', '}', '-'], "_")),
            "parameters": params,
            "responses": {
                "200": {"description": "Success"},
                "400": {"$ref": "#/components/responses/Error"},
                "401": {"$ref": "#/components/responses/Error"},
                "403": {"$ref": "#/components/responses/Error"},
                "404": {"$ref": "#/components/responses/Error"},
                "429": {"$ref": "#/components/responses/Error"}
            }
        });
        if o.path.starts_with("/api/v1/sms/send") || o.path == "/api/v1/sms/schedule" {
            operation["responses"]["202"] = json!({"description": "Accepted and queued", "content": {"application/json": {"example": {"success": true, "message_id": "example-uuid", "status": "queued", "encoding": "gsm7", "parts": 1}}}});
        }
        if let Some(example) = o.body {
            let ex: Value = serde_json::from_str(example).unwrap_or(Value::Null);
            operation["requestBody"] = json!({"required": true, "content": {"application/json": {"schema": {"type": "object"}, "example": ex}}});
        }
        if o.admin {
            operation["description"] = json!("Administrative endpoint: subject to api.admin_enabled, api.admin_allowed_cidrs and api.admin_tokens.");
            operation["x-admin"] = json!(true);
        }
        paths.entry(o.path.to_string()).or_insert_with(|| json!({}))[o.method] = operation;
    }
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "THUGSred SMS Gateway API",
            "version": crate::daemon::VERSION,
            "description": "REST API of THUGSred-smsGateway. Responses are JSON objects with a `success` flag; errors carry `error.code` and `error.message`. Every response includes an `X-Request-Id` header.",
            "license": {"name": "MIT"},
            "contact": {"name": "THUGSred", "url": "https://thugs.red"}
        },
        "servers": [{"url": "/", "description": "This gateway"}],
        "paths": paths,
        "components": {
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer", "description": "Only required when api.authentication = \"bearer\""}},
            "responses": {"Error": {"description": "Error", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}},
            "schemas": {
                "Error": {"type": "object", "properties": {"success": {"type": "boolean", "example": false}, "error": {"type": "object", "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}}}
            }
        },
        "security": [{}, {"bearer": []}]
    })
}

pub async fn serve() -> Json<Value> {
    Json(document())
}

#[cfg(test)]
mod tests {
    #[test]
    fn document_is_well_formed() {
        let d = super::document();
        assert_eq!(d["openapi"], "3.0.3");
        assert!(d["paths"]["/api/v1/sms/send"]["post"]["requestBody"].is_object());
        assert!(d["paths"]["/api/v1/messages/{id}"]["delete"]["x-admin"]
            .as_bool()
            .unwrap());
        assert!(d["paths"].as_object().unwrap().len() > 40);
    }
}
