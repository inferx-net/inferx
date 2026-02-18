use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::FromRow;

use crate::common::*;

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct Apikey {
    #[serde(default)]
    pub key_id: i64,
    pub apikey: String,
    pub username: String,
    pub keyname: String,
    #[serde(rename = "access_level")]
    pub access_level: String,
    pub restrict_tenant: Option<String>,
    pub restrict_namespace: Option<String>,
    pub createtime: Option<chrono::NaiveDateTime>,
    pub expires_at: Option<chrono::NaiveDateTime>,
    pub revoked_at: Option<chrono::NaiveDateTime>,
    pub revoked_by: Option<String>,
    pub revoke_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct Role {
    pub rolename: String,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct User {
    pub username: String,
}

#[derive(Debug, Clone)]
pub struct SqlSecret {
    pub pool: PgPool,
}

impl SqlSecret {
    pub async fn New(sqlSvcAddr: &str) -> Result<Self> {
        let url_parts = url::Url::parse(sqlSvcAddr).expect("Failed to parse URL");
        let username = url_parts.username();
        let password = url_parts.password().unwrap_or("");
        let host = url_parts.host_str().unwrap_or("localhost");
        let port = url_parts.port().unwrap_or(5432);
        let database = url_parts.path().trim_start_matches('/');

        let options = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password)
            .database(database);

        options.clone().disable_statement_logging();

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        return Ok(Self { pool: pool });
    }

    pub async fn CreateApikey(&self, key: &Apikey) -> Result<()> {
        let query = "insert into Apikey (
                apikey,
                username,
                keyname,
                access_level,
                restrict_tenant,
                restrict_namespace,
                createtime,
                expires_at,
                revoked_at,
                revoked_by,
                revoke_reason
            ) values (
                $1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9, $10
            )";

        let _result = sqlx::query(query)
            .bind(&key.apikey)
            .bind(&key.username)
            .bind(&key.keyname)
            .bind(&key.access_level)
            .bind(&key.restrict_tenant)
            .bind(&key.restrict_namespace)
            .bind(&key.expires_at)
            .bind(&key.revoked_at)
            .bind(&key.revoked_by)
            .bind(&key.revoke_reason)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn GetApikey(&self, apikey: &str) -> Result<Apikey> {
        let query = "select
                key_id,
                apikey,
                username,
                keyname,
                access_level,
                restrict_tenant,
                restrict_namespace,
                createtime,
                expires_at,
                revoked_at,
                revoked_by,
                revoke_reason
            from Apikey where apikey = $1";
        let key = sqlx::query_as::<_, Apikey>(query)
            .bind(apikey)
            .fetch_one(&self.pool)
            .await?;
        return Ok(key);
    }

    pub async fn GetApikeys(&self, username: &str) -> Result<Vec<Apikey>> {
        let query = "select
                key_id,
                apikey,
                username,
                keyname,
                access_level,
                restrict_tenant,
                restrict_namespace,
                createtime,
                expires_at,
                revoked_at,
                revoked_by,
                revoke_reason
            from Apikey where username = $1 order by key_id";
        let keys = sqlx::query_as::<_, Apikey>(query)
            .bind(username)
            .fetch_all(&self.pool)
            .await?;
        return Ok(keys);
    }

    pub async fn DeleteApikey(&self, keyname: &str, username: &str) -> Result<Vec<String>> {
        let query = "delete from Apikey where keyname = $1 and username = $2 returning apikey";
        let keys = sqlx::query_scalar::<_, String>(query)
            .bind(keyname)
            .bind(username)
            .fetch_all(&self.pool)
            .await?;
        return Ok(keys);
    }

    pub async fn AddRole(&self, username: &str, role: &str) -> Result<()> {
        let query = "insert into UserRole (username, rolename) values \
        ($1, $2)";

        let _result = sqlx::query(query)
            .bind(username)
            .bind(role)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn GetRoles(&self, username: &str) -> Result<Vec<String>> {
        let query = format!(
            "select rolename from UserRole where username = '{}'",
            username
        );

        let selectQuery = sqlx::query_as::<_, Role>(&query);
        let roles: Vec<Role> = selectQuery.fetch_all(&self.pool).await?;
        let mut v = Vec::new();
        for r in roles {
            v.push(r.rolename);
        }
        return Ok(v);
    }

    pub async fn DeleteRole(&self, username: &str, role: &str) -> Result<()> {
        let query = format!(
            "delete from UserRole where username = '{}' and rolename='{}'",
            username, role
        );
        let result = sqlx::query(&query).execute(&self.pool).await;

        match result {
            Err(e) => {
                error!("Error DeleteRole: {}\n", e.to_string());
                return Err(e.into());
            }

            Ok(_res) => return Ok(()),
        }
    }

    pub async fn DeleteRolesByUsername(&self, username: &str) -> Result<()> {
        let query = format!("delete from UserRole where username = '{}'", username);
        let result = sqlx::query(&query).execute(&self.pool).await;

        match result {
            Err(e) => {
                error!("Error DeleteRolesByUsername: {}\n", e.to_string());
                return Err(e.into());
            }

            Ok(_res) => return Ok(()),
        }
    }

    pub async fn GetTenantAdmins(&self, tenant: &str) -> Result<Vec<String>> {
        let query = format!(
            "select username from UserRole where rolename = '/tenant/admin/{}'",
            tenant
        );

        let selectQuery = sqlx::query_as::<_, User>(&query);
        let roles: Vec<User> = selectQuery.fetch_all(&self.pool).await?;
        let mut v = Vec::new();
        for r in roles {
            v.push(r.username);
        }
        return Ok(v);
    }

    pub async fn GetTenantUsers(&self, tenant: &str) -> Result<Vec<String>> {
        let query = format!(
            "select username from UserRole where rolename = '/tenant/user/{}'",
            tenant
        );

        let selectQuery = sqlx::query_as::<_, User>(&query);
        let roles: Vec<User> = selectQuery.fetch_all(&self.pool).await?;
        let mut v = Vec::new();
        for r in roles {
            v.push(r.username);
        }
        return Ok(v);
    }

    pub async fn GetNamespaceAdmins(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        let query = format!(
            "select username from UserRole where rolename = '/namespace/admin/{}/{}'",
            tenant, namespace
        );

        let selectQuery = sqlx::query_as::<_, User>(&query);
        let roles: Vec<User> = selectQuery.fetch_all(&self.pool).await?;
        let mut v = Vec::new();
        for r in roles {
            v.push(r.username);
        }
        return Ok(v);
    }

    pub async fn GetNamespaceUsers(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        let query = format!(
            "select username from UserRole where rolename = '/namespace/user/{}/{}'",
            tenant, namespace
        );

        let selectQuery = sqlx::query_as::<_, User>(&query);
        let roles: Vec<User> = selectQuery.fetch_all(&self.pool).await?;
        let mut v = Vec::new();
        for r in roles {
            v.push(r.username);
        }
        return Ok(v);
    }
}
