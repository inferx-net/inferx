use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::FromRow;
use sqlx::Row;

use crate::common::*;

pub trait OwnSkillToolIdentity {
    fn owner_tenant(&self) -> &str;
    fn owner_namespace(&self) -> &str;
    fn skillname(&self) -> &str;
}

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

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct OnboardInfo {
    pub sub: String,
    pub username: String,
    pub tenant_name: String,
    pub status: String,
    pub saga_step: i32,
    pub onboarded_at: Option<chrono::NaiveDateTime>,
    pub completed_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct TenantProfile {
    pub tenant_name: String,
    pub sub: String,
    pub display_name: Option<String>,
    pub email: String,
    pub created_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EndpointMetadata {
    pub brief_intro: Option<String>,
    pub detailed_intro: Option<String>,
    pub cs_ttft: Option<String>,
    #[serde(default)]
    pub recommended_use_cases: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub provider: Option<String>,
    pub parameter_count_b: Option<f64>,
    pub context_length: Option<i64>,
    pub concurrency: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillTemplate {
    pub template_id: i64,
    pub tenant: String,
    pub namespace: String,
    pub display_name: String,
    pub description: Option<String>,
    pub func_tenant: String,
    pub func_namespace: String,
    pub normal_funcname: String,
    pub producer_funcname: Option<String>,
    pub producer_revision: Option<i64>,
    pub consumer_funcname: Option<String>,
    pub consumer_revision: Option<i64>,
    pub is_active: bool,
    pub created_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillRecord {
    pub skill_id: i64,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub description: Option<String>,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub inferx_revenue_share_pct: f64,
    pub active_revision_id: Option<i64>,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub published_by: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillRevisionRecord {
    pub revision_id: i64,
    pub skill_id: i64,
    pub version: i32,
    pub template_id: i64,
    pub has_cache: bool,
    pub cache_status: String,
    pub cache_ready_at: Option<chrono::NaiveDateTime>,
    pub created_at: Option<chrono::NaiveDateTime>,
    pub created_by: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillDetail {
    pub skill_id: i64,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub description: Option<String>,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub inferx_revenue_share_pct: f64,
    pub active_revision_id: Option<i64>,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub published_by: Option<String>,
    pub revision_id: i64,
    pub version: i32,
    pub template_id: i64,
    pub has_cache: bool,
    pub cache_status: String,
    pub cache_ready_at: Option<chrono::NaiveDateTime>,
    pub revision_created_at: Option<chrono::NaiveDateTime>,
    pub revision_created_by: String,
    pub template_tenant: String,
    pub template_namespace: String,
    pub template_display_name: String,
    pub template_description: Option<String>,
    pub func_tenant: String,
    pub func_namespace: String,
    pub normal_funcname: String,
    pub producer_funcname: Option<String>,
    pub producer_revision: Option<i64>,
    pub consumer_funcname: Option<String>,
    pub consumer_revision: Option<i64>,
    pub template_is_active: bool,
    pub template_created_at: Option<chrono::NaiveDateTime>,
    pub allowed_child_skilleps: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillSummary {
    pub skill_id: i64,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub description: Option<String>,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub active_revision_id: Option<i64>,
    pub version: i32,
    pub has_cache: bool,
    pub cache_status: String,
    pub template_display_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillMarketplaceItem {
    pub skill_id: i64,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub description: Option<String>,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub active_revision_id: Option<i64>,
    pub version: i32,
    pub has_cache: bool,
    pub cache_status: String,
    pub template_display_name: String,
    pub is_subscribed: bool,
    pub tool_alias: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SkillMarketplacePage {
    pub items: Vec<SkillMarketplaceItem>,
    pub page: i64,
    pub page_size: i64,
    pub has_next: bool,
    pub keyword: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillSubscription {
    pub subscription_id: i64,
    pub subscriber_tenant: String,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub tool_alias: String,
    pub subscribed_at: chrono::DateTime<chrono::Utc>,
    pub subscribed_by: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SkillSubscriptionWithDetail {
    pub subscription_id: i64,
    pub subscriber_tenant: String,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub tool_alias: String,
    pub subscribed_at: chrono::DateTime<chrono::Utc>,
    pub subscribed_by: String,
    pub skill_id: i64,
    pub description: Option<String>,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub inferx_revenue_share_pct: f64,
    pub active_revision_id: Option<i64>,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub published_by: Option<String>,
    pub revision_id: i64,
    pub version: i32,
    pub template_id: i64,
    pub has_cache: bool,
    pub cache_status: String,
    pub cache_ready_at: Option<chrono::NaiveDateTime>,
    pub revision_created_at: Option<chrono::NaiveDateTime>,
    pub revision_created_by: String,
    pub template_tenant: String,
    pub template_namespace: String,
    pub template_display_name: String,
    pub template_description: Option<String>,
    pub func_tenant: String,
    pub func_namespace: String,
    pub normal_funcname: String,
    pub producer_funcname: Option<String>,
    pub producer_revision: Option<i64>,
    pub consumer_funcname: Option<String>,
    pub consumer_revision: Option<i64>,
    pub template_is_active: bool,
    pub template_created_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct OwnSkillToolRoute {
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub description: Option<String>,
}

impl OwnSkillToolIdentity for OwnSkillToolRoute {
    fn owner_tenant(&self) -> &str {
        &self.owner_tenant
    }

    fn owner_namespace(&self) -> &str {
        &self.owner_namespace
    }

    fn skillname(&self) -> &str {
        &self.skillname
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SubscribedSkillRoute {
    pub tool_name: String,
    pub description: Option<String>,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TenantToolRoute {
    pub tool_name: String,
    pub description: Option<String>,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
}

#[derive(Debug, Clone)]
pub struct SqlSecret {
    pub pool: PgPool,
}

fn percent_encode_tool_name_component(component: &str) -> String {
    let mut encoded = String::with_capacity(component.len());
    for byte in component.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~');
        if keep {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{:02X}", byte));
        }
    }
    encoded
}

pub fn compute_own_skill_tool_names<T: OwnSkillToolIdentity>(
    skills: &[T],
) -> HashMap<(String, String, String), String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for skill in skills {
        *counts.entry(skill.skillname()).or_insert(0) += 1;
    }

    let mut names = HashMap::new();
    for skill in skills {
        let tool_name = if skill.skillname().contains('.')
            || counts.get(skill.skillname()).copied().unwrap_or(0) > 1
        {
            format!(
                "{}.{}.{}",
                percent_encode_tool_name_component(skill.skillname()),
                percent_encode_tool_name_component(skill.owner_namespace()),
                percent_encode_tool_name_component(skill.owner_tenant())
            )
        } else {
            skill.skillname().to_string()
        };
        names.insert(
            (
                skill.owner_tenant().to_string(),
                skill.owner_namespace().to_string(),
                skill.skillname().to_string(),
            ),
            tool_name,
        );
    }

    names
}

impl OwnSkillToolIdentity for SkillDetail {
    fn owner_tenant(&self) -> &str {
        &self.owner_tenant
    }

    fn owner_namespace(&self) -> &str {
        &self.owner_namespace
    }

    fn skillname(&self) -> &str {
        &self.skillname
    }
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

    pub async fn GetOnboardInfo(&self, sub: &str) -> Result<Option<OnboardInfo>> {
        let query = "select
                sub,
                username,
                tenant_name,
                status,
                saga_step,
                onboarded_at,
                completed_at
            from UserOnboard
            where sub = $1";

        let info = sqlx::query_as::<_, OnboardInfo>(query)
            .bind(sub)
            .fetch_optional(&self.pool)
            .await?;
        return Ok(info);
    }

    pub async fn InsertOnboard(
        &self,
        sub: &str,
        username: &str,
        tenant_name: &str,
    ) -> Result<bool> {
        let query = "insert into UserOnboard (
                sub, username, tenant_name, status, saga_step
            ) values (
                $1, $2, $3, 'pending', 0
            ) on conflict (sub) do nothing";

        let res = sqlx::query(query)
            .bind(sub)
            .bind(username)
            .bind(tenant_name)
            .execute(&self.pool)
            .await?;

        return Ok(res.rows_affected() == 1);
    }

    pub async fn UpdateOnboardStep(&self, sub: &str, saga_step: i32) -> Result<()> {
        let query = "update UserOnboard
            set saga_step = GREATEST(saga_step, $2)
            where sub = $1";

        let _res = sqlx::query(query)
            .bind(sub)
            .bind(saga_step)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn CompleteOnboard(&self, sub: &str) -> Result<()> {
        let query = "update UserOnboard
            set status = 'complete',
                saga_step = GREATEST(saga_step, 3),
                completed_at = NOW()
            where sub = $1";

        let _res = sqlx::query(query).bind(sub).execute(&self.pool).await?;
        return Ok(());
    }

    pub async fn UpsertTenantProfile(
        &self,
        tenant_name: &str,
        sub: &str,
        display_name: &Option<String>,
        email: &str,
    ) -> Result<()> {
        let query = "insert into TenantProfile (
                tenant_name, sub, display_name, email
            ) values (
                $1, $2, $3, $4
            ) on conflict (sub) do update
            set display_name = EXCLUDED.display_name,
                email = EXCLUDED.email,
                updated_at = NOW()";

        let _res = sqlx::query(query)
            .bind(tenant_name)
            .bind(sub)
            .bind(display_name)
            .bind(email)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn GetTenantProfilesByTenantNames(
        &self,
        tenant_names: &[String],
    ) -> Result<HashMap<String, TenantProfile>> {
        if tenant_names.is_empty() {
            return Ok(HashMap::new());
        }

        let names = tenant_names.to_vec();
        let query = "select
                tenant_name,
                sub,
                display_name,
                email::text as email,
                created_at
            from TenantProfile
            where tenant_name = any($1)";

        let rows = sqlx::query_as::<_, TenantProfile>(query)
            .bind(names)
            .fetch_all(&self.pool)
            .await?;

        let mut map = HashMap::new();
        for row in rows {
            map.insert(row.tenant_name.clone(), row);
        }
        return Ok(map);
    }

    pub async fn CompleteOnboardWithProfile(
        &self,
        sub: &str,
        tenant_name: &str,
        display_name: &Option<String>,
        email: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        let upsert_query = "insert into TenantProfile (
                tenant_name, sub, display_name, email
            ) values (
                $1, $2, $3, $4
            ) on conflict (sub) do update
            set display_name = EXCLUDED.display_name,
                email = EXCLUDED.email,
                updated_at = NOW()";

        let _res = sqlx::query(upsert_query)
            .bind(tenant_name)
            .bind(sub)
            .bind(display_name)
            .bind(email)
            .execute(&mut *tx)
            .await?;

        let complete_query = "update UserOnboard
            set status = 'complete',
                saga_step = GREATEST(saga_step, 3),
                completed_at = NOW()
            where sub = $1";

        let _res = sqlx::query(complete_query)
            .bind(sub)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        return Ok(());
    }

    pub async fn MarkOnboardFailed(&self, sub: &str) -> Result<()> {
        let query = "update UserOnboard
            set status = 'failed'
            where sub = $1";

        let _res = sqlx::query(query).bind(sub).execute(&self.pool).await?;
        return Ok(());
    }

    pub async fn ResetOnboard(&self, sub: &str) -> Result<()> {
        let query = "update UserOnboard
            set status = 'pending'
            where sub = $1";

        let _res = sqlx::query(query).bind(sub).execute(&self.pool).await?;
        return Ok(());
    }

    pub async fn DeleteOnboard(&self, sub: &str) -> Result<()> {
        let query = "delete from UserOnboard where sub = $1";
        let _res = sqlx::query(query).bind(sub).execute(&self.pool).await?;
        return Ok(());
    }

    pub async fn UpdateOnboardUsername(&self, sub: &str, username: &str) -> Result<()> {
        let query = "update UserOnboard
            set username = $2
            where sub = $1";

        let _res = sqlx::query(query)
            .bind(sub)
            .bind(username)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn UpsertEndpointMetadata(
        &self,
        slug: &str,
        func_revision: i64,
        metadata: &EndpointMetadata,
    ) -> Result<()> {
        let query = r#"
            INSERT INTO Endpoints (
                slug,
                func_revision,
                brief_intro,
                detailed_intro,
                cs_ttft,
                recommended_use_cases,
                tags,
                provider,
                parameter_count_b,
                context_length,
                concurrency
            ) VALUES (
                $1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8, $9, $10, $11
            )
            ON CONFLICT (slug)
            DO UPDATE SET
                brief_intro = EXCLUDED.brief_intro,
                detailed_intro = EXCLUDED.detailed_intro,
                cs_ttft = EXCLUDED.cs_ttft,
                recommended_use_cases = EXCLUDED.recommended_use_cases,
                tags = EXCLUDED.tags,
                provider = EXCLUDED.provider,
                parameter_count_b = EXCLUDED.parameter_count_b,
                context_length = EXCLUDED.context_length,
                concurrency = EXCLUDED.concurrency
        "#;

        sqlx::query(query)
            .bind(slug)
            .bind(func_revision)
            .bind(&metadata.brief_intro)
            .bind(&metadata.detailed_intro)
            .bind(&metadata.cs_ttft)
            .bind(serde_json::to_value(&metadata.recommended_use_cases)?)
            .bind(serde_json::to_value(&metadata.tags)?)
            .bind(&metadata.provider)
            .bind(metadata.parameter_count_b)
            .bind(metadata.context_length)
            .bind(metadata.concurrency)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn PublishEndpoint(
        &self,
        slug: &str,
        func_revision: i64,
        metadata: &EndpointMetadata,
        last_published_by: &str,
    ) -> Result<()> {
        let query = r#"
            INSERT INTO Endpoints (
                slug,
                func_revision,
                brief_intro,
                detailed_intro,
                cs_ttft,
                recommended_use_cases,
                tags,
                provider,
                parameter_count_b,
                context_length,
                concurrency,
                last_published_at,
                last_published_by
            ) VALUES (
                $1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8, $9, $10, $11, NOW(), $12
            )
            ON CONFLICT (slug)
            DO UPDATE SET
                func_revision = EXCLUDED.func_revision,
                brief_intro = EXCLUDED.brief_intro,
                detailed_intro = EXCLUDED.detailed_intro,
                cs_ttft = EXCLUDED.cs_ttft,
                recommended_use_cases = EXCLUDED.recommended_use_cases,
                tags = EXCLUDED.tags,
                provider = EXCLUDED.provider,
                parameter_count_b = EXCLUDED.parameter_count_b,
                context_length = EXCLUDED.context_length,
                concurrency = EXCLUDED.concurrency,
                last_published_at = NOW(),
                last_published_by = EXCLUDED.last_published_by
        "#;

        sqlx::query(query)
            .bind(slug)
            .bind(func_revision)
            .bind(&metadata.brief_intro)
            .bind(&metadata.detailed_intro)
            .bind(&metadata.cs_ttft)
            .bind(serde_json::to_value(&metadata.recommended_use_cases)?)
            .bind(serde_json::to_value(&metadata.tags)?)
            .bind(&metadata.provider)
            .bind(metadata.parameter_count_b)
            .bind(metadata.context_length)
            .bind(metadata.concurrency)
            .bind(last_published_by)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn ListActiveSkillTemplates(&self) -> Result<Vec<SkillTemplate>> {
        let query = r#"
            SELECT
                template_id,
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                producer_funcname,
                producer_revision,
                consumer_funcname,
                consumer_revision,
                is_active,
                created_at
            FROM SkillTemplate
            WHERE is_active = TRUE
            ORDER BY tenant, namespace, display_name
        "#;

        Ok(sqlx::query_as::<_, SkillTemplate>(query)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn ListSkillTemplates(&self) -> Result<Vec<SkillTemplate>> {
        let query = r#"
            SELECT
                template_id,
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                producer_funcname,
                producer_revision,
                consumer_funcname,
                consumer_revision,
                is_active,
                created_at
            FROM SkillTemplate
            ORDER BY tenant, namespace, display_name, template_id
        "#;

        Ok(sqlx::query_as::<_, SkillTemplate>(query)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn ListSkillsByTenant(
        &self,
        owner_tenant: &str,
        show_all: bool,
    ) -> Result<Vec<SkillSummary>> {
        let mut query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.is_published,
                s.published_at,
                s.active_revision_id,
                sr.version,
                sr.has_cache,
                sr.cache_status,
                st.display_name AS template_display_name
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
            WHERE s.owner_tenant = $1
        "#
        .to_string();

        if !show_all {
            query.push_str("\n              AND s.is_published = TRUE");
        }
        query.push_str("\n            ORDER BY s.owner_namespace, s.skillname");

        Ok(sqlx::query_as::<_, SkillSummary>(&query)
            .bind(owner_tenant)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn ListSkillsByNamespace(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        show_all: bool,
    ) -> Result<Vec<SkillSummary>> {
        let mut query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.is_published,
                s.published_at,
                s.active_revision_id,
                sr.version,
                sr.has_cache,
                sr.cache_status,
                st.display_name AS template_display_name
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
            WHERE s.owner_tenant = $1
              AND s.owner_namespace = $2
        "#
        .to_string();

        if !show_all {
            query.push_str("\n              AND s.is_published = TRUE");
        }
        query.push_str("\n            ORDER BY s.owner_namespace, s.skillname");

        Ok(sqlx::query_as::<_, SkillSummary>(&query)
            .bind(owner_tenant)
            .bind(owner_namespace)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn ListPublishedSkills(&self) -> Result<Vec<SkillSummary>> {
        self.ListSkillsFiltered(true).await
    }

    pub async fn ListAllSkills(&self) -> Result<Vec<SkillSummary>> {
        self.ListSkillsFiltered(false).await
    }

    async fn ListSkillsFiltered(&self, published_only: bool) -> Result<Vec<SkillSummary>> {
        let mut query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.is_published,
                s.published_at,
                s.active_revision_id,
                sr.version,
                sr.has_cache,
                sr.cache_status,
                st.display_name AS template_display_name
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
        "#
        .to_string();

        if published_only {
            query.push_str("\n            WHERE s.is_published = TRUE");
        }
        query.push_str("\n            ORDER BY s.owner_tenant, s.owner_namespace, s.skillname");

        Ok(sqlx::query_as::<_, SkillSummary>(&query)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn GetSkillTemplate(&self, template_id: i64) -> Result<SkillTemplate> {
        let query = r#"
            SELECT
                template_id,
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                producer_funcname,
                producer_revision,
                consumer_funcname,
                consumer_revision,
                is_active,
                created_at
            FROM SkillTemplate
            WHERE template_id = $1
        "#;

        Ok(sqlx::query_as::<_, SkillTemplate>(query)
            .bind(template_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn ListMarketplaceSkills(
        &self,
        subscriber_tenant: Option<&str>,
        keyword: Option<&str>,
        page: i64,
        page_size: i64,
        include_unpublished: bool,
    ) -> Result<SkillMarketplacePage> {
        let keyword = keyword
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let page = page.max(1);
        let page_size = page_size.max(1);
        let offset = (page - 1) * page_size;
        let limit = page_size + 1;

        let mut query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.is_published,
                s.published_at,
                s.active_revision_id,
                sr.version,
                sr.has_cache,
                sr.cache_status,
                st.display_name AS template_display_name,
                CASE WHEN ss.subscription_id IS NULL THEN FALSE ELSE TRUE END AS is_subscribed,
                ss.tool_alias
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
            LEFT JOIN SkillSubscription ss
                ON ss.owner_tenant = s.owner_tenant
               AND ss.owner_namespace = s.owner_namespace
               AND ss.skillname = s.skillname
               AND ss.subscriber_tenant = $1
            WHERE TRUE
        "#
        .to_string();

        if !include_unpublished {
            query.push_str("\n              AND s.is_published = TRUE");
        }

        if keyword.is_some() {
            query.push_str(
                r#"
              AND (
                    s.skillname ILIKE $2
                 OR s.owner_tenant ILIKE $2
                 OR s.owner_namespace ILIKE $2
                 OR COALESCE(s.description, '') ILIKE $2
                 OR st.display_name ILIKE $2
                 OR COALESCE(st.description, '') ILIKE $2
              )
            "#,
            );
        }

        query.push_str("\n            ORDER BY s.owner_tenant, s.owner_namespace, s.skillname");
        if keyword.is_some() {
            query.push_str("\n            LIMIT $3 OFFSET $4");
        } else {
            query.push_str("\n            LIMIT $2 OFFSET $3");
        }

        let rows = if let Some(keyword) = keyword.as_deref() {
            sqlx::query_as::<_, SkillMarketplaceItem>(&query)
                .bind(subscriber_tenant)
                .bind(format!("%{}%", keyword))
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query_as::<_, SkillMarketplaceItem>(&query)
                .bind(subscriber_tenant)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() as i64 > page_size;
        let items = rows.into_iter().take(page_size as usize).collect();

        Ok(SkillMarketplacePage {
            items,
            page,
            page_size,
            has_next,
            keyword,
        })
    }

    pub async fn IsSkillTemplateReferenced(&self, template_id: i64) -> Result<bool> {
        let row = sqlx::query(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM SkillRevision
                    WHERE template_id = $1
                )
            "#,
        )
        .bind(template_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get::<bool, _>(0))
    }

    pub async fn CreateSkillTemplate(
        &self,
        tenant: &str,
        namespace: &str,
        display_name: &str,
        description: Option<&str>,
        func_tenant: &str,
        func_namespace: &str,
        normal_funcname: &str,
        is_active: bool,
    ) -> Result<SkillTemplate> {
        let query = r#"
            INSERT INTO SkillTemplate (
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                is_active
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING
                template_id,
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                producer_funcname,
                producer_revision,
                consumer_funcname,
                consumer_revision,
                is_active,
                created_at
        "#;

        Ok(sqlx::query_as::<_, SkillTemplate>(query)
            .bind(tenant)
            .bind(namespace)
            .bind(display_name)
            .bind(description)
            .bind(func_tenant)
            .bind(func_namespace)
            .bind(normal_funcname)
            .bind(is_active)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn DeactivateSkillTemplate(&self, template_id: i64) -> Result<()> {
        let result = sqlx::query(
            r#"
                UPDATE SkillTemplate
                SET is_active = FALSE
                WHERE template_id = $1
            "#,
        )
        .bind(template_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "skill template {} does not exist",
                template_id
            )));
        }

        Ok(())
    }

    pub async fn ActivateSkillTemplate(&self, template_id: i64) -> Result<()> {
        let result = sqlx::query(
            r#"
                UPDATE SkillTemplate
                SET is_active = TRUE
                WHERE template_id = $1
            "#,
        )
        .bind(template_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "skill template {} does not exist",
                template_id
            )));
        }

        Ok(())
    }

    pub async fn UpdateSkillTemplate(
        &self,
        template_id: i64,
        tenant: &str,
        namespace: &str,
        display_name: &str,
        description: Option<&str>,
        func_tenant: &str,
        func_namespace: &str,
        normal_funcname: &str,
        is_active: bool,
    ) -> Result<SkillTemplate> {
        let query = r#"
            UPDATE SkillTemplate
            SET
                tenant = $2,
                namespace = $3,
                display_name = $4,
                description = $5,
                func_tenant = $6,
                func_namespace = $7,
                normal_funcname = $8,
                is_active = $9
            WHERE template_id = $1
            RETURNING
                template_id,
                tenant,
                namespace,
                display_name,
                description,
                func_tenant,
                func_namespace,
                normal_funcname,
                producer_funcname,
                producer_revision,
                consumer_funcname,
                consumer_revision,
                is_active,
                created_at
        "#;

        let row = sqlx::query_as::<_, SkillTemplate>(query)
            .bind(template_id)
            .bind(tenant)
            .bind(namespace)
            .bind(display_name)
            .bind(description)
            .bind(func_tenant)
            .bind(func_namespace)
            .bind(normal_funcname)
            .bind(is_active)
            .fetch_optional(&self.pool)
            .await?;

        row.ok_or_else(|| {
            Error::NotExist(format!("skill template {} does not exist", template_id))
        })
    }

    pub async fn DeleteSkillTemplate(&self, template_id: i64) -> Result<()> {
        let result = sqlx::query(
            r#"
                DELETE FROM SkillTemplate
                WHERE template_id = $1
            "#,
        )
        .bind(template_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "skill template {} does not exist",
                template_id
            )));
        }

        Ok(())
    }

    pub async fn CreateSkill(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
        description: Option<&str>,
        serving_mode: &str,
        earning_type: &str,
        user_price_microcents: Option<i32>,
        gpu_billing_target: &str,
        template_id: i64,
        has_cache: bool,
        allowed_child_skilleps: Option<&[String]>,
        created_by: &str,
    ) -> Result<SkillDetail> {
        let mut tx = self.pool.begin().await?;

        let skill_row = sqlx::query(
            r#"
                INSERT INTO Skill (
                    owner_tenant,
                    owner_namespace,
                    skillname,
                    description,
                    serving_mode,
                    earning_type,
                    user_price_microcents,
                    gpu_billing_target
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                RETURNING skill_id
            "#,
        )
        .bind(owner_tenant)
        .bind(owner_namespace)
        .bind(skillname)
        .bind(description)
        .bind(serving_mode)
        .bind(earning_type)
        .bind(user_price_microcents)
        .bind(gpu_billing_target)
        .fetch_one(&mut *tx)
        .await?;
        let skill_id: i64 = skill_row.try_get("skill_id")?;

        let rev_row = sqlx::query(
            r#"
                INSERT INTO SkillRevision (
                    skill_id,
                    version,
                    template_id,
                    has_cache,
                    cache_status,
                    allowed_child_skilleps,
                    created_by
                ) VALUES ($1, 1, $2, $3, 'none', $4, $5)
                RETURNING revision_id
            "#,
        )
        .bind(skill_id)
        .bind(template_id)
        .bind(has_cache)
        .bind(allowed_child_skilleps)
        .bind(created_by)
        .fetch_one(&mut *tx)
        .await?;
        let revision_id: i64 = rev_row.try_get("revision_id")?;

        sqlx::query(
            r#"
                UPDATE Skill
                SET active_revision_id = $1
                WHERE skill_id = $2
            "#,
        )
        .bind(revision_id)
        .bind(skill_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.GetSkill(owner_tenant, owner_namespace, skillname).await
    }

    pub async fn ListSkillDetailsByTenant(&self, owner_tenant: &str) -> Result<Vec<SkillDetail>> {
        let query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.inferx_revenue_share_pct::FLOAT8 AS inferx_revenue_share_pct,
                s.active_revision_id,
                s.is_published,
                s.published_at,
                s.published_by,
                sr.revision_id,
                sr.version,
                sr.template_id,
                sr.has_cache,
                sr.cache_status,
                sr.cache_ready_at,
                sr.created_at AS revision_created_at,
                sr.created_by AS revision_created_by,
                st.tenant AS template_tenant,
                st.namespace AS template_namespace,
                st.display_name AS template_display_name,
                st.description AS template_description,
                st.func_tenant,
                st.func_namespace,
                st.normal_funcname,
                st.producer_funcname,
                st.producer_revision,
                st.consumer_funcname,
                st.consumer_revision,
                st.is_active AS template_is_active,
                st.created_at AS template_created_at,
                sr.allowed_child_skilleps
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
            WHERE s.owner_tenant = $1
            ORDER BY s.skillname, s.owner_namespace
        "#;

        Ok(sqlx::query_as::<_, SkillDetail>(query)
            .bind(owner_tenant)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn GetSkill(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
    ) -> Result<SkillDetail> {
        let query = r#"
            SELECT
                s.skill_id,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.inferx_revenue_share_pct::FLOAT8 AS inferx_revenue_share_pct,
                s.active_revision_id,
                s.is_published,
                s.published_at,
                s.published_by,
                sr.revision_id,
                sr.version,
                sr.template_id,
                sr.has_cache,
                sr.cache_status,
                sr.cache_ready_at,
                sr.created_at AS revision_created_at,
                sr.created_by AS revision_created_by,
                st.tenant AS template_tenant,
                st.namespace AS template_namespace,
                st.display_name AS template_display_name,
                st.description AS template_description,
                st.func_tenant,
                st.func_namespace,
                st.normal_funcname,
                st.producer_funcname,
                st.producer_revision,
                st.consumer_funcname,
                st.consumer_revision,
                st.is_active AS template_is_active,
                st.created_at AS template_created_at,
                sr.allowed_child_skilleps
            FROM Skill s
            JOIN SkillRevision sr
                ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
                ON st.template_id = sr.template_id
            WHERE s.owner_tenant = $1
              AND s.owner_namespace = $2
              AND s.skillname = $3
        "#;

        Ok(sqlx::query_as::<_, SkillDetail>(query)
            .bind(owner_tenant)
            .bind(owner_namespace)
            .bind(skillname)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn PublishSkill(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
        published_by: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
                UPDATE Skill
                SET is_published = TRUE,
                    published_at = NOW(),
                    published_by = $4
                WHERE owner_tenant = $1
                  AND owner_namespace = $2
                  AND skillname = $3
            "#,
        )
        .bind(owner_tenant)
        .bind(owner_namespace)
        .bind(skillname)
        .bind(published_by)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "skill {}/{}/{} does not exist",
                owner_tenant, owner_namespace, skillname
            )));
        }
        Ok(())
    }

    pub async fn UnpublishSkill(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
                UPDATE Skill
                SET is_published = FALSE
                WHERE owner_tenant = $1
                  AND owner_namespace = $2
                  AND skillname = $3
            "#,
        )
        .bind(owner_tenant)
        .bind(owner_namespace)
        .bind(skillname)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "skill {}/{}/{} does not exist",
                owner_tenant, owner_namespace, skillname
            )));
        }
        Ok(())
    }

    pub async fn DeleteSkill(
        &self,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
    ) -> Result<()> {
        let detail = self.GetSkill(owner_tenant, owner_namespace, skillname).await?;
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
                UPDATE Skill
                SET active_revision_id = NULL
                WHERE skill_id = $1
            "#,
        )
        .bind(detail.skill_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM SkillRevision WHERE skill_id = $1")
            .bind(detail.skill_id)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM Skill WHERE skill_id = $1")
            .bind(detail.skill_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn CreateSkillSubscription(
        &self,
        subscriber_tenant: &str,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
        tool_alias: &str,
        subscribed_by: &str,
        allow_unpublished: bool,
    ) -> Result<SkillSubscription> {
        let query = r#"
            INSERT INTO SkillSubscription (
                subscriber_tenant,
                owner_tenant,
                owner_namespace,
                skillname,
                tool_alias,
                subscribed_by
            )
            SELECT $1, s.owner_tenant, s.owner_namespace, s.skillname, $5, $6
            FROM Skill s
            WHERE s.owner_tenant = $2
              AND s.owner_namespace = $3
              AND s.skillname = $4
              AND (
                    s.is_published = TRUE
                    OR s.owner_tenant = $1
                    OR $7 = TRUE
              )
            RETURNING
                subscription_id,
                subscriber_tenant,
                owner_tenant,
                owner_namespace,
                skillname,
                tool_alias,
                subscribed_at,
                subscribed_by
        "#;

        let row = sqlx::query_as::<_, SkillSubscription>(query)
            .bind(subscriber_tenant)
            .bind(owner_tenant)
            .bind(owner_namespace)
            .bind(skillname)
            .bind(tool_alias)
            .bind(subscribed_by)
            .bind(allow_unpublished)
            .fetch_optional(&self.pool)
            .await?;

        row.ok_or_else(|| {
            Error::NotExist(format!(
                "published skill {}/{}/{} does not exist, or the unpublished skill is not owned by subscriber tenant {}",
                owner_tenant, owner_namespace, skillname
                , subscriber_tenant
            ))
        })
    }

    pub async fn GetSubscribedSkillRoute(
        &self,
        subscriber_tenant: &str,
        tool_alias: &str,
    ) -> Result<Option<SubscribedSkillRoute>> {
        let query = r#"
            SELECT
                ss.tool_alias AS tool_name,
                s.description,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname
            FROM SkillSubscription ss
            JOIN Skill s
              ON s.owner_tenant = ss.owner_tenant
             AND s.owner_namespace = ss.owner_namespace
             AND s.skillname = ss.skillname
            WHERE ss.subscriber_tenant = $1
              AND ss.tool_alias = $2
            LIMIT 1
        "#;

        Ok(sqlx::query_as::<_, SubscribedSkillRoute>(query)
            .bind(subscriber_tenant)
            .bind(tool_alias)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn GetToolsForTenant(&self, tenant: &str) -> Result<Vec<TenantToolRoute>> {
        let subscribed_query = r#"
            SELECT
                ss.tool_alias AS tool_name,
                s.description,
                s.owner_tenant,
                s.owner_namespace,
                s.skillname
            FROM SkillSubscription ss
            JOIN Skill s
              ON s.owner_tenant = ss.owner_tenant
             AND s.owner_namespace = ss.owner_namespace
             AND s.skillname = ss.skillname
            WHERE ss.subscriber_tenant = $1
            ORDER BY ss.tool_alias, s.owner_tenant, s.owner_namespace, s.skillname
        "#;

        let subscribed = sqlx::query_as::<_, SubscribedSkillRoute>(subscribed_query)
            .bind(tenant)
            .fetch_all(&self.pool)
            .await?;

        Ok(subscribed
            .into_iter()
            .map(|tool| TenantToolRoute {
                tool_name: tool.tool_name,
                description: tool.description,
                owner_tenant: tool.owner_tenant,
                owner_namespace: tool.owner_namespace,
                skillname: tool.skillname,
            })
            .collect())
    }

    pub async fn ListSkillSubscriptions(
        &self,
        subscriber_tenant: &str,
    ) -> Result<Vec<SkillSubscriptionWithDetail>> {
        let query = r#"
            SELECT
                ss.subscription_id,
                ss.subscriber_tenant,
                ss.owner_tenant,
                ss.owner_namespace,
                ss.skillname,
                ss.tool_alias,
                ss.subscribed_at,
                ss.subscribed_by,
                s.skill_id,
                s.description,
                s.serving_mode,
                s.earning_type,
                s.user_price_microcents,
                s.gpu_billing_target,
                s.inferx_revenue_share_pct::FLOAT8 AS inferx_revenue_share_pct,
                s.active_revision_id,
                s.is_published,
                s.published_at,
                s.published_by,
                sr.revision_id,
                sr.version,
                sr.template_id,
                sr.has_cache,
                sr.cache_status,
                sr.cache_ready_at,
                sr.created_at AS revision_created_at,
                sr.created_by AS revision_created_by,
                st.tenant AS template_tenant,
                st.namespace AS template_namespace,
                st.display_name AS template_display_name,
                st.description AS template_description,
                st.func_tenant,
                st.func_namespace,
                st.normal_funcname,
                st.producer_funcname,
                st.producer_revision,
                st.consumer_funcname,
                st.consumer_revision,
                st.is_active AS template_is_active,
                st.created_at AS template_created_at
            FROM SkillSubscription ss
            JOIN Skill s
              ON s.owner_tenant = ss.owner_tenant
             AND s.owner_namespace = ss.owner_namespace
             AND s.skillname = ss.skillname
            JOIN SkillRevision sr
              ON sr.revision_id = s.active_revision_id
            JOIN SkillTemplate st
              ON st.template_id = sr.template_id
            WHERE ss.subscriber_tenant = $1
            ORDER BY ss.tool_alias, ss.owner_tenant, ss.owner_namespace, ss.skillname
        "#;

        Ok(sqlx::query_as::<_, SkillSubscriptionWithDetail>(query)
            .bind(subscriber_tenant)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn DeleteSkillSubscription(
        &self,
        subscriber_tenant: &str,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
                DELETE FROM SkillSubscription
                WHERE subscriber_tenant = $1
                  AND owner_tenant = $2
                  AND owner_namespace = $3
                  AND skillname = $4
            "#,
        )
        .bind(subscriber_tenant)
        .bind(owner_tenant)
        .bind(owner_namespace)
        .bind(skillname)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotExist(format!(
                "subscription {}/{}/{} does not exist for tenant {}",
                owner_tenant, owner_namespace, skillname, subscriber_tenant
            )));
        }

        Ok(())
    }

    pub async fn UpdateSkillSubscriptionAlias(
        &self,
        subscriber_tenant: &str,
        owner_tenant: &str,
        owner_namespace: &str,
        skillname: &str,
        tool_alias: &str,
    ) -> Result<SkillSubscription> {
        let query = r#"
            UPDATE SkillSubscription
            SET tool_alias = $5
            WHERE subscriber_tenant = $1
              AND owner_tenant = $2
              AND owner_namespace = $3
              AND skillname = $4
            RETURNING
                subscription_id,
                subscriber_tenant,
                owner_tenant,
                owner_namespace,
                skillname,
                tool_alias,
                subscribed_at,
                subscribed_by
        "#;

        let row = sqlx::query_as::<_, SkillSubscription>(query)
            .bind(subscriber_tenant)
            .bind(owner_tenant)
            .bind(owner_namespace)
            .bind(skillname)
            .bind(tool_alias)
            .fetch_optional(&self.pool)
            .await?;

        row.ok_or_else(|| {
            Error::NotExist(format!(
                "subscription {}/{}/{} does not exist for tenant {}",
                owner_tenant, owner_namespace, skillname, subscriber_tenant
            ))
        })
    }
}
