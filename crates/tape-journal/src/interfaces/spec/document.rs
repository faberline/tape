//! The hand-rolled OpenAPI document and JSON schemas.

use serde_json::{json, Value};

pub(super) fn openapi() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Tape API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Topic replay journal API for append, replay, subscription resource inventory, checkpoints, retention, and standard service endpoints."
        },
        "paths": {
            "/healthz": {"get": {"summary": "Liveness probe", "responses": ok_text()}},
            "/readyz": {"get": {"summary": "Readiness probe", "responses": ok_text()}},
            "/metrics": {"get": {"summary": "Prometheus metrics", "responses": ok_text()}},
            "/openapi.json": {"get": {"summary": "OpenAPI document", "responses": ok_json()}},
            "/docs": {"get": {"summary": "Swagger UI", "responses": ok_text()}},
            "/admin/backup": {
                "get": {
                    "summary": "Download an admin-gated whole-journal snapshot",
                    "responses": {
                        "200": {
                            "description": "JournalSnapshot JSON containing the journal at the applied Raft index",
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object"}
                                }
                            }
                        }
                    }
                }
            },
            "/topics/{topic}/append": {
                "post": {
                    "summary": "Append an event envelope to a topic journal",
                    "parameters": [topic_param()],
                    "requestBody": json_body("AppendEventRequest"),
                    "responses": mutating_schema("TapeEvent")
                }
            },
            "/topics/{topic}/replay": {
                "get": {
                    "summary": "Replay topic history by offset or timestamp",
                    "parameters": [
                        topic_param(),
                        query_param("from_offset", "integer"),
                        query_param("from_timestamp_ms", "integer"),
                        query_param("limit", "integer")
                    ],
                    "responses": ok_schema("ReplayResponse")
                }
            },
            "/topics/{topic}/replay/stream": {
                "get": {
                    "summary": "Download compact read-only topic replay frames",
                    "parameters": [
                        topic_param(),
                        query_param("from_offset", "integer"),
                        query_param("from_timestamp_ms", "integer"),
                        query_param("limit", "integer")
                    ],
                    "responses": {
                        "200": {
                            "description": "Length-framed Tape replay stream",
                            "content": {
                                "application/vnd.tape.replay.v1": {
                                    "schema": {"type": "string", "contentEncoding": "binary"}
                                }
                            }
                        }
                    }
                }
            },
            "/topics/{topic}/subscriptions": {
                "post": {
                    "summary": "Create a topic delivery resource",
                    "parameters": [topic_param()],
                    "requestBody": json_body("SubscriptionCreateRequest"),
                    "responses": mutating_schema("Subscription")
                },
                "get": {
                    "summary": "List topic delivery resources",
                    "parameters": [topic_param()],
                    "responses": ok_schema("SubscriptionListResponse")
                }
            },
            "/topics/{topic}/subscriptions/{subscription}": {
                "get": {
                    "summary": "Inspect one topic delivery resource",
                    "parameters": [topic_param(), subscription_param()],
                    "responses": ok_schema("Subscription")
                },
                "delete": {
                    "summary": "Delete topic delivery resource metadata",
                    "parameters": [topic_param(), subscription_param()],
                    "responses": mutating_schema("Subscription")
                }
            },
            "/topics/{topic}/subscriptions/{subscription}/pull": {
                "post": {
                    "summary": "Read a bounded pull subscription window",
                    "parameters": [topic_param(), subscription_param()],
                    "requestBody": json_body("PullSubscriptionRequest"),
                    "responses": ok_schema("PullSubscriptionBatch")
                }
            },
            "/topics/{topic}/subscriptions/{subscription}/ack": {
                "post": {
                    "summary": "Advance a pull subscription cursor",
                    "parameters": [topic_param(), subscription_param()],
                    "requestBody": json_body("PullSubscriptionAckRequest"),
                    "responses": mutating_schema("ConsumerCheckpoint")
                }
            },
            "/topics/{topic}/consumers/{consumer}/checkpoint": {
                "get": {
                    "summary": "Read a consumer replay checkpoint",
                    "parameters": [topic_param(), consumer_param()],
                    "responses": ok_schema("ConsumerCheckpoint")
                },
                "put": {
                    "summary": "Advance a consumer replay checkpoint",
                    "parameters": [topic_param(), consumer_param()],
                    "requestBody": json_body("CheckpointRequest"),
                    "responses": mutating_schema("ConsumerCheckpoint")
                }
            },
            "/topics/{topic}/retention": {
                "get": {
                    "summary": "Read a topic's current retention window",
                    "parameters": [topic_param()],
                    "responses": ok_schema("RetentionPolicy")
                },
                "put": {
                    "summary": "Configure a topic retention window",
                    "parameters": [topic_param()],
                    "requestBody": json_body("RetentionPolicy"),
                    "responses": mutating_schema("RetentionPolicy")
                }
            }
        },
        "components": {
            "schemas": schemas()
        }
    })
}

pub(super) fn schemas() -> Value {
    json!({
        "AppendEventRequest": {
            "type": "object",
            "required": ["payload"],
            "properties": {
                "key": {"type": "string"},
                "timestamp_ms": {"type": "integer", "minimum": 0},
                "payload": {"description": "Caller-owned event envelope or claim-check reference"}
            }
        },
        "TapeEvent": {
            "type": "object",
            "required": ["topic", "offset", "timestamp_ms", "payload"],
            "properties": {
                "topic": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0},
                "timestamp_ms": {"type": "integer", "minimum": 0},
                "key": {"type": "string"},
                "payload": {}
            }
        },
        "ReplayResponse": {
            "type": "object",
            "required": ["events"],
            "properties": {
                "events": {"type": "array", "items": {"$ref": "#/components/schemas/TapeEvent"}}
            }
        },
        "SubscriptionCreateRequest": {
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "properties": {
                "name": {"type": "string"}
            }
        },
        "Subscription": {
            "type": "object",
            "required": ["topic", "name"],
            "properties": {
                "topic": {"type": "string"},
                "name": {"type": "string"}
            }
        },
        "SubscriptionListResponse": {
            "type": "object",
            "required": ["subscriptions"],
            "properties": {
                "subscriptions": {"type": "array", "items": {"$ref": "#/components/schemas/Subscription"}}
            }
        },
        "PullSubscriptionRequest": {
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 0, "maximum": 1000, "default": 100}
            }
        },
        "PullSubscriptionBatch": {
            "type": "object",
            "required": ["topic", "subscription", "cursor", "limit", "next_offset", "events"],
            "properties": {
                "topic": {"type": "string"},
                "subscription": {"type": "string"},
                "cursor": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 0, "maximum": 1000},
                "next_offset": {"type": "integer", "minimum": 0},
                "events": {"type": "array", "items": {"$ref": "#/components/schemas/TapeEvent"}}
            }
        },
        "PullSubscriptionAckRequest": {
            "type": "object",
            "required": ["offset"],
            "properties": {
                "offset": {"type": "integer", "minimum": 0}
            }
        },
        "CheckpointRequest": {
            "type": "object",
            "required": ["offset"],
            "properties": {
                "offset": {"type": "integer", "minimum": 0}
            }
        },
        "ConsumerCheckpoint": {
            "type": "object",
            "required": ["topic", "consumer", "offset", "updated_at_ms"],
            "properties": {
                "topic": {"type": "string"},
                "consumer": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0},
                "updated_at_ms": {"type": "integer", "minimum": 0}
            }
        },
        "RetentionPolicy": {
            "type": "object",
            "properties": {
                "min_offset": {"type": "integer", "minimum": 0},
                "max_age_seconds": {"type": "integer", "minimum": 0},
                "protected_consumers": {"type": "array", "items": {"type": "string"}}
            }
        }
    })
}

fn topic_param() -> Value {
    path_param("topic", "Topic name")
}

fn consumer_param() -> Value {
    path_param("consumer", "Consumer checkpoint name")
}

fn subscription_param() -> Value {
    path_param("subscription", "Topic delivery resource name")
}

fn path_param(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "description": description,
        "schema": {"type": "string"}
    })
}

fn query_param(name: &str, kind: &str) -> Value {
    json!({
        "name": name,
        "in": "query",
        "required": false,
        "schema": {"type": kind}
    })
}

fn json_body(schema: &str) -> Value {
    json!({
        "required": true,
        "content": {
            "application/json": {
                "schema": {"$ref": format!("#/components/schemas/{schema}")}
            }
        }
    })
}

fn ok_schema(schema: &str) -> Value {
    json!({
        "200": {
            "description": "ok",
            "content": {
                "application/json": {
                    "schema": {"$ref": format!("#/components/schemas/{schema}")}
                }
            }
        }
    })
}

/// #2573: the response set for an operation that writes through the journal
/// persist path.
///
/// This document otherwise lists only success responses, and 4xx stays out of
/// it deliberately — a client already handles those by status class. `507` is
/// different in kind: it is the one status where the correct client behavior
/// (stop, surface the condition, retry on a human timescale) differs from what
/// a generic retry policy would do with a 5xx, so a generated client that does
/// not know it exists will hammer a full disk. Read-only operations never
/// reach the persist path and so never carry it.
fn mutating_schema(schema: &str) -> Value {
    let mut responses = ok_schema(schema);
    responses["507"] = json!({
        "description": "Node is in ENOSPC degraded read-only mode (error kind `storage_full`); \
                        reads keep serving and the node re-probes its store to recover itself"
    });
    responses
}

fn ok_json() -> Value {
    json!({"200": {"description": "ok", "content": {"application/json": {"schema": {"type": "object"}}}}})
}

fn ok_text() -> Value {
    json!({"200": {"description": "ok", "content": {"text/plain": {"schema": {"type": "string"}}}}})
}
