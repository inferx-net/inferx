use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::FromRow;

use crate::common::*;

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct Apikey {
    pub apikey: String,
    pub username: String,
    pub keyname: String,
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

    pub async fn CreateApikey(&self, apikey: &str, username: &str, keyname: &str) -> Result<()> {
        let query = "insert into Apikey (apikey, username, keyname, createtime) values \
        ($1, $2, $3, NOW())";

        let _result = sqlx::query(query)
            .bind(apikey)
            .bind(username)
            .bind(keyname)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn GetApikey(&self, apikey: &str) -> Result<Apikey> {
        let query = format!(
            "select apikey, username, keyname \
                from Apikey where apikey = '{}'",
            apikey
        );
        let selectQuery = sqlx::query_as::<_, Apikey>(&query);
        let key: Apikey = selectQuery.fetch_one(&self.pool).await?;
        return Ok(key);
    }

    pub async fn GetApikeys(&self, username: &str) -> Result<Vec<Apikey>> {
        let query = format!(
            "select apikey, username, keyname \
                from Apikey where username= '{}'",
            username
        );
        let selectQuery = sqlx::query_as::<_, Apikey>(&query);
        let keys: Vec<Apikey> = selectQuery.fetch_all(&self.pool).await?;
        return Ok(keys);
    }

    pub async fn DeleteApikey(&self, keyname: &str, username: &str) -> bool {
        let query = format!(
            "delete from Apikey where keyname = '{}' and username='{}'",
            keyname, username
        );
        let result = sqlx::query(&query).execute(&self.pool).await;

        match result {
            Err(e) => {
                error!("Error deleting apikey: {}\n", e.to_string());
                return false;
            }

            Ok(res) => {
                return res.rows_affected() > 0;
            }
        }
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
