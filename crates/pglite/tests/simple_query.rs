use futures::executor::block_on;
use pglite::{Error, PGlite};
use postgres_types::Type;

#[test]
fn simple_query_covers_results_multi_statement_and_errors() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();

        assert!(db.simple_query("").await.unwrap().is_empty());

        let ddl = db
            .simple_query("CREATE TABLE t (id int PRIMARY KEY, name text)")
            .await
            .unwrap();
        assert_eq!(ddl.len(), 1);
        assert_eq!(ddl[0].command_tag, "CREATE TABLE");
        assert!(ddl[0].columns.is_empty());
        assert!(ddl[0].rows.is_empty());

        let insert = db
            .simple_query("INSERT INTO t VALUES (1, 'alice'), (2, 'bob')")
            .await
            .unwrap();
        assert_eq!(insert[0].command_tag, "INSERT 0 2");
        assert!(insert[0].columns.is_empty());
        assert!(insert[0].rows.is_empty());

        let update = db
            .simple_query("UPDATE t SET name = upper(name)")
            .await
            .unwrap();
        assert_eq!(update[0].command_tag, "UPDATE 2");

        let empty = db
            .simple_query("SELECT id, name, ARRAY[1, 2, 3]::int4[] AS nums FROM t WHERE false")
            .await
            .unwrap();
        assert_eq!(
            empty[0].columns,
            vec![
                ("id".into(), Type::INT4),
                ("name".into(), Type::TEXT),
                ("nums".into(), Type::INT4_ARRAY),
            ]
        );
        assert!(empty[0].rows.is_empty());

        let values = db
            .simple_query(
                "
                SELECT
                    1::int2 AS i2,
                    2::int4 AS i4,
                    3::int8 AS i8,
                    1.25::float4 AS f4,
                    2.5::float8 AS f8,
                    decode('6869', 'hex') AS bytes,
                    NULL::text AS empty,
                    ARRAY[1, 2, 3]::int4[] AS nums,
                    true AS ok
                ",
            )
            .await
            .unwrap();
        assert_eq!(values[0].command_tag, "SELECT 1");
        assert_eq!(
            values[0].rows[0],
            vec![
                Some("1".into()),
                Some("2".into()),
                Some("3".into()),
                Some("1.25".into()),
                Some("2.5".into()),
                Some("\\x6869".into()),
                None,
                Some("{1,2,3}".into()),
                Some("t".into()),
            ]
        );

        let zero_columns = db.simple_query("SELECT").await.unwrap();
        assert_eq!(zero_columns[0].command_tag, "SELECT 1");
        assert!(zero_columns[0].columns.is_empty());
        assert_eq!(zero_columns[0].rows, vec![Vec::<Option<String>>::new()]);

        let multi = db
            .simple_query("SELECT 1 AS n; SELECT 2 AS n")
            .await
            .unwrap();
        let [first, second] = multi.as_slice() else {
            panic!("expected two result sets, got {multi:?}");
        };
        assert_eq!(first.command_tag, "SELECT 1");
        assert_eq!(first.columns, vec![("n".into(), Type::INT4)]);
        assert_eq!(first.rows[0][0].as_deref(), Some("1"));
        assert_eq!(second.command_tag, "SELECT 1");
        assert_eq!(second.columns, vec![("n".into(), Type::INT4)]);
        assert_eq!(second.rows[0][0].as_deref(), Some("2"));

        let mixed = db
            .simple_query("CREATE TABLE m (id int); INSERT INTO m VALUES (1); SELECT id FROM m")
            .await
            .unwrap();
        assert_eq!(mixed.len(), 3);
        assert_eq!(mixed[0].command_tag, "CREATE TABLE");
        assert_eq!(mixed[1].command_tag, "INSERT 0 1");
        assert_eq!(mixed[2].command_tag, "SELECT 1");
        assert_eq!(mixed[2].rows[0][0].as_deref(), Some("1"));

        let err = db
            .simple_query("SELECT * FROM does_not_exist")
            .await
            .unwrap_err();
        match &err {
            Error::Database {
                sqlstate, message, ..
            } => {
                assert_eq!(sqlstate, "42P01");
                assert!(message.contains("does_not_exist"), "{message}");
            }
            other => panic!("expected undefined table error, got {other:?}"),
        }

        let err = db.simple_query("SELECT FROM").await.unwrap_err();
        match &err {
            Error::Database { sqlstate, .. } => assert_eq!(sqlstate, "42601"),
            other => panic!("expected syntax error, got {other:?}"),
        }

        let err = db.simple_query("SELECT 1 / 0").await.unwrap_err();
        match &err {
            Error::Database {
                sqlstate, message, ..
            } => {
                assert_eq!(sqlstate, "22012");
                assert!(message.contains("division by zero"), "{message}");
            }
            other => panic!("expected division by zero error, got {other:?}"),
        }

        let err = db
            .simple_query("SELECT 1; SELECT * FROM does_not_exist")
            .await
            .unwrap_err();
        match &err {
            Error::Database {
                sqlstate, message, ..
            } => {
                assert_eq!(sqlstate, "42P01");
                assert!(message.contains("does_not_exist"), "{message}");
            }
            other => panic!("expected multi-statement failure, got {other:?}"),
        }

        let err = db
            .simple_query("COPY (SELECT 1) TO STDOUT")
            .await
            .unwrap_err();
        match &err {
            Error::Protocol(message) => {
                assert!(
                    message.contains("unsupported message during simple query"),
                    "{message}"
                );
            }
            other => panic!("expected unsupported copy error, got {other:?}"),
        }

        let ok = db.simple_query("SELECT 42").await.unwrap();
        assert_eq!(ok[0].rows[0][0].as_deref(), Some("42"));

        // A simple-query roundtrip must consume the whole backend response so the
        // next extended-query roundtrip still starts on a clean protocol boundary.
        db.simple_query("SELECT 1").await.unwrap();
        db.query("SELECT 1", &[]).await.unwrap();

        db.close().await.unwrap();
    });
}
