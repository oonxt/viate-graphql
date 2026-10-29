//! A manually mapped TimeTz on a text-backed database is deliberately text,
//! not an emulation of PostgreSQL's offset-aware comparison rules.

use serde_json::json;
use viate_graphql::schema::{ColumnType, Table};
use viate_graphql::{Backend, Dialect, Engine, Schema};

pub fn schema(dialect: Dialect, physical_schema: &str) -> Schema {
    Schema::builder()
        .dialect(dialect)
        .table(
            Table::new("shifts", physical_schema, "shifts")
                .column("id", "id", ColumnType::Int4, false)
                .column("starts_at", "starts_at", ColumnType::TimeTz, true)
                .primary_key(&["id"]),
        )
        .build()
}

pub async fn check<DB: Backend>(engine: Engine<DB>) {
    let early = "08:00:00-03:30";
    let late = "09:00:00.123456+02";
    let value = engine
        .query(
            "mutation($early: timetz!, $late: timetz!) {
                insert_shifts(objects: [{id: 1, starts_at: $late}, {id: 2, starts_at: $early}]) {
                    affected_rows
                }
            }",
            Some(json!({"early": early, "late": late})),
        )
        .await
        .unwrap();
    assert_eq!(value["insert_shifts"]["affected_rows"], 2);
    let value = engine
        .query(
            "{ shifts(order_by: {starts_at: asc}) { id starts_at } }",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        value["shifts"],
        json!([
            {"id": 2, "starts_at": early}, {"id": 1, "starts_at": late}
        ])
    );

    for (declaration, operator, input) in [
        ("timetz!", "_eq", json!(late)),
        ("timetz!", "_gt", json!(early)),
        ("[timetz!]!", "_in", json!([late])),
    ] {
        let query = engine
            .compile(&format!(
                "query($value: {declaration}) {{
                shifts(where: {{starts_at: {{{operator}: $value}}}}) {{ id starts_at }}
            }}"
            ))
            .unwrap();
        let value = engine
            .execute(&query, Some(json!({"value": input})))
            .await
            .unwrap();
        assert_eq!(value["shifts"], json!([{"id": 1, "starts_at": late}]));
    }
    let update = engine
        .compile(
            "mutation($time: timetz) {
            update_shifts(where: {id: {_eq: 1}}, _set: {starts_at: $time}) {
                returning { starts_at }
            }
        }",
        )
        .unwrap();
    for time in [json!(null), json!("00:00:00.5+05:45")] {
        let value = engine
            .execute(&update, Some(json!({"time": time})))
            .await
            .unwrap();
        assert_eq!(
            value["update_shifts"]["returning"],
            json!([{"starts_at": time}])
        );
    }
}
