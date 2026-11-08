//! One PgDog serving two databases on the same Postgres server, one in
//! session mode and one in transaction mode.

use pgdog_config::{Database, PoolerMode, User};

use crate::{
    backend::databases::init,
    config::{set, ConfigAndUsers},
    net::{DataRow, Message},
};

use super::prelude::*;

const SESSION_DB: &str = "session_db";
const TRANSACTION_DB: &str = "transaction_db";

/// `session_db` -> Postgres database `pgdog`, `transaction_db` -> `shard_0`,
/// both on 127.0.0.1:5432. `user_pooler_mode` is set on the user for both.
fn configure(user_pooler_mode: Option<PoolerMode>) {
    let database = |name: &str, database_name: &str, pooler_mode| Database {
        name: name.into(),
        host: "127.0.0.1".into(),
        port: 5432,
        database_name: Some(database_name.into()),
        pooler_mode: Some(pooler_mode),
        ..Default::default()
    };
    let user = |database: &str| User {
        name: "pgdog".into(),
        database: database.into(),
        password: Some("pgdog".into()),
        pooler_mode: user_pooler_mode,
        ..Default::default()
    };

    let mut config = ConfigAndUsers::default();
    config.config.databases = vec![
        database(SESSION_DB, "pgdog", PoolerMode::Session),
        database(TRANSACTION_DB, "shard_0", PoolerMode::Transaction),
    ];
    config.users.users = vec![user(SESSION_DB), user(TRANSACTION_DB)];
    set(config).unwrap();
    init().unwrap();
}

async fn client(database: &str) -> TestClient {
    let mut params = Parameters::default();
    params.insert("database", database);
    TestClient::new(params).await
}

async fn value(client: &mut TestClient, query: &str) -> String {
    client.send_simple(Query::new(query)).await;
    client
        .read_until('Z')
        .await
        .unwrap()
        .into_iter()
        .find(|message: &Message| message.code() == 'D')
        .and_then(|message| DataRow::try_from(message).ok())
        .and_then(|row| row.get_text(0))
        .unwrap()
}

#[tokio::test]
async fn test_each_database_uses_its_own_pooler_mode() {
    configure(None);
    let mut session = client(SESSION_DB).await;
    let mut transaction = client(TRANSACTION_DB).await;

    // Each logical database reaches its own Postgres database.
    assert_eq!(
        value(&mut session, "SELECT current_database()").await,
        "pgdog"
    );
    assert_eq!(
        value(&mut transaction, "SELECT current_database()").await,
        "shard_0"
    );

    // Session mode: the client keeps its server connection between statements.
    let first_pid = value(&mut session, "SELECT pg_backend_pid()").await;
    assert!(
        session.backend_connected(),
        "session client released its connection"
    );
    let second_pid = value(&mut session, "SELECT pg_backend_pid()").await;
    assert_eq!(
        first_pid, second_pid,
        "session client switched server connections"
    );

    // Transaction mode: the connection goes back to the pool after each transaction.
    value(&mut transaction, "SELECT pg_backend_pid()").await;
    assert!(
        !transaction.backend_connected(),
        "transaction client kept its connection after the transaction"
    );
}

/// Session state stays with its client: a temporary table created by one
/// session client is visible to it later, and never to another client.
#[tokio::test]
async fn test_session_state_stays_with_its_client() {
    configure(None);
    let mut owner = client(SESSION_DB).await;
    let mut other = client(SESSION_DB).await;

    owner
        .send_simple(Query::new("CREATE TEMPORARY TABLE session_scoped (id INT)"))
        .await;
    owner.read_until('Z').await.unwrap();

    let visible = "SELECT to_regclass('pg_temp.session_scoped') IS NOT NULL";
    assert_eq!(
        value(&mut other, visible).await,
        "f",
        "another client saw the session's state"
    );
    assert_eq!(
        value(&mut owner, visible).await,
        "t",
        "the session lost its state"
    );
}

/// A pooler mode set on the user wins over the database's.
#[tokio::test]
async fn test_user_pooler_mode_overrides_database() {
    configure(Some(PoolerMode::Transaction));
    let mut session = client(SESSION_DB).await;

    value(&mut session, "SELECT 1").await;

    assert!(
        !session.backend_connected(),
        "database pooler_mode = session should be overridden by the user's transaction mode"
    );
}
