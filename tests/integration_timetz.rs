//! `timetz` must stay a database time-with-offset, not become a text or a
//! zone-less time at a bind, projection, or pagination boundary.

use serde_json::{json, Value};
use vision_graphql::ast::OrderDir;
use vision_graphql::schema::ColumnType;
use vision_graphql::{Engine, Query};

mod common;

async fn setup() -> (Engine, common::TestDb) {
    let db = common::fresh_db().await;
    sqlx::raw_sql(
        "CREATE TABLE shifts (id INT PRIMARY KEY, starts_at TIMETZ(6));
         INSERT INTO shifts VALUES
             (1, '09:00:00.123456+02'),
             (2, '08:00:00+00'),
             (3, '07:30:00-02'),
             (4, NULL);",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let schema = vision_graphql::Schema::introspect(&db.pool)
        .await
        .unwrap()
        .build();
    (Engine::new(db.pool.clone(), schema), db)
}

fn ids(value: &Value, root: &str) -> Vec<i64> {
    value[root]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn timetz_orders_by_database_time_not_wall_clock_or_text() {
    let (engine, _db) = setup().await;
    let value = engine
        .query(
            "{ shifts(order_by: {starts_at: asc_nulls_last}) { id starts_at } }",
            None,
        )
        .await
        .unwrap();
    assert_eq!(ids(&value, "shifts"), [1, 2, 3, 4]);
    assert_eq!(value["shifts"][0]["starts_at"], "09:00:00.123456+02");
    assert_eq!(value["shifts"][3]["starts_at"], Value::Null);

    let value = engine
        .query(
            "{ shifts(order_by: {starts_at: desc_nulls_last}, limit: 2, offset: 1) { id } }",
            None,
        )
        .await
        .unwrap();
    assert_eq!(ids(&value, "shifts"), [2, 1]);

    let value = engine
        .run(
            Query::from("shifts")
                .select(&["id"])
                .order_by("starts_at", OrderDir::Asc)
                .limit(2),
        )
        .await
        .unwrap();
    assert_eq!(ids(&value, "shifts"), [1, 2]);
}

#[tokio::test]
async fn timetz_introspection_includes_views_and_publishes_its_own_scalar() {
    let (engine, db) = setup().await;
    sqlx::raw_sql(
        "CREATE VIEW shift_view AS SELECT starts_at FROM shifts;
         CREATE MATERIALIZED VIEW shift_snapshot AS SELECT starts_at FROM shifts;",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let introspected = vision_graphql::schema::introspect::introspect(&db.pool)
        .await
        .unwrap();
    assert!(introspected.skipped_columns.is_empty());
    for table in ["shifts", "shift_view", "shift_snapshot"] {
        let column = introspected.tables[&("public".into(), table.into())]
            .columns
            .iter()
            .find(|c| c.name == "starts_at")
            .unwrap();
        assert_eq!(column.ty, ColumnType::TimeTz);
    }
    let schema = vision_graphql::Schema::introspect(&db.pool)
        .await
        .unwrap()
        .build();
    let ts = vision_graphql::type_system::TypeSystem::build(&schema);
    let sdl = vision_graphql::sdl::render(&ts);
    assert!(sdl.contains("scalar timetz\n"), "{sdl}");
    assert!(sdl.contains("starts_at: timetz"), "{sdl}");
    assert!(sdl.contains("input timetz_comparison_exp"), "{sdl}");

    let value = engine
        .query(
            "{ shifts_aggregate { aggregate { min { starts_at } max { starts_at } } } }",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        value["shifts_aggregate"]["aggregate"],
        json!({"min": {"starts_at": "09:00:00.123456+02"},
               "max": {"starts_at": "07:30:00-02"}})
    );
}

#[tokio::test]
async fn timetz_compiled_scalar_and_list_filters_keep_the_offset() {
    let (engine, _db) = setup().await;
    let source = "query($after: timetz!) {
        shifts(where: {starts_at: {_gt: $after}}, order_by: {starts_at: asc}) { id }
    }";
    let compiled = engine.compile(source).unwrap();
    for (after, expected) in [("09:00:00.123456+02", vec![2, 3]), ("08:00:00+00", vec![3])] {
        let vars = json!({"after": after});
        let value = engine.execute(&compiled, Some(vars.clone())).await.unwrap();
        assert_eq!(ids(&value, "shifts"), expected);
        assert_eq!(value, engine.query(source, Some(vars)).await.unwrap());
    }

    let compiled = engine
        .compile(
            "query($times: [timetz!]!) {
            shifts(where: {starts_at: {_in: $times}}, order_by: {starts_at: asc}) { id }
        }",
        )
        .unwrap();
    for (times, expected) in [
        (json!(["09:00:00.123456+02", "07:30:00-02"]), vec![1, 3]),
        (json!([]), vec![]),
        // Unlike timestamptz, timetz breaks equal UTC clock times by offset.
        (json!(["07:00:00.123456+00"]), vec![]),
    ] {
        let value = engine
            .execute(&compiled, Some(json!({"times": times})))
            .await
            .unwrap();
        assert_eq!(ids(&value, "shifts"), expected);
    }

    let value = engine
        .query(
            "{ shifts(where: {starts_at: {_is_null: true}}) { id } }",
            None,
        )
        .await
        .unwrap();
    assert_eq!(ids(&value, "shifts"), [4]);
    assert!(engine
        .compile("{ shifts(where: {starts_at: {_eq: 12}}) { id } }")
        .is_err());
}

#[tokio::test]
async fn timetz_compiled_writes_round_trip_nulls_offsets_and_fractions() {
    let (engine, _db) = setup().await;
    let insert = "mutation($id: Int!, $time: timetz) {
        insert_shifts(objects: [{id: $id, starts_at: $time}]) {
            returning { id starts_at }
        }
    }";
    for (id, time) in [(5, Value::Null), (6, json!("23:59:59.123456-03:30"))] {
        let value = engine
            .query(insert, Some(json!({"id": id, "time": time})))
            .await
            .unwrap();
        assert_eq!(
            value["insert_shifts"]["returning"],
            json!([{"id": id, "starts_at": time}])
        );
    }
    let update = engine
        .compile(
            "mutation($time: timetz) {
            update_shifts(where: {id: {_eq: 5}}, _set: {starts_at: $time}) {
                returning { starts_at }
            }
        }",
        )
        .unwrap();
    let time = "00:00:00.5+05:45";
    for input in [Value::Null, json!(time)] {
        let value = engine
            .execute(&update, Some(json!({"time": input})))
            .await
            .unwrap();
        assert_eq!(
            value["update_shifts"]["returning"],
            json!([{"starts_at": input}])
        );
    }
    let value = engine
        .query(
            "query($time: timetz!) { shifts(where: {starts_at: {_eq: $time}}) { id } }",
            Some(json!({"time": time})),
        )
        .await
        .unwrap();
    assert_eq!(ids(&value, "shifts"), [5]);
}
