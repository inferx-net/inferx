use hyper::StatusCode;
use inferxlib::data_obj::DataObject;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use std::time::Duration;

use inferxlib::common::*;
use serde_json::Value;

use crate::command::{
    Apikey, ApikeyCreateRequest, ApikeyCreateResponse, ApikeyDeleteRequest, Grant, RoleBinding,
    UserRole,
};

pub struct ObjectClient {
    pub url: String,
}

impl ObjectClient {
    pub fn New(url: &str) -> Self {
        return Self {
            url: url.to_owned(),
        };
    }

    pub fn Client(&self) -> Client {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        return client;
    }

    pub async fn Get(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<DataObject<Value>> {
        let url = format!(
            "{}/object/{objType}/{tenant}/{namespace}/{name}/",
            &self.url
        );
        let body = self.Client().get(&url).send().await.unwrap().text().await?;
        let obj = serde_json::from_str(&body)?;
        return Ok(obj);
    }

    pub async fn List(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        let client = self.Client();

        let url = format!("{}/objects/{objType}/{tenant}/{namespace}/", &self.url);
        error!("url is {:?}", &url);
        let body = client.get(&url).send().await?.text().await?;
        let obj = serde_json::from_str(&body)?;
        return Ok(obj);
    }

    pub async fn Create(&self, token: &str, obj: DataObject<Value>) -> Result<i64> {
        let client = self.Client();
        let url = format!("{}/object/", &self.url);
        println!("Create url {}", &url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }
        let resp = client.put(&url).headers(headers).json(&obj).send().await?;
        let code = resp.status().as_u16();
        if code == StatusCode::OK {
            let res = resp.text().await?;
            match res.parse::<i64>() {
                Err(e) => {
                    return Err(Error::CommonError(format!(
                        "can't parse res with error {:?}",
                        e
                    )))
                }
                Ok(version) => return Ok(version),
            }
        }

        let content = resp.text().await?;
        return Err(Error::CommonError(format!(
            "Create fail with resp {}",
            content
        )));
    }

    pub async fn Update(&self, token: &str, obj: DataObject<Value>) -> Result<i64> {
        let client = self.Client();
        let url = format!("{}/object/", &self.url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }
        let resp = client.post(&url).headers(headers).json(&obj).send().await?;
        let code = resp.status().as_u16();
        if code == StatusCode::OK {
            let res = resp.text().await?;
            match res.parse::<i64>() {
                Err(e) => {
                    return Err(Error::CommonError(format!(
                        "can't parse res with error {:?}",
                        e
                    )))
                }
                Ok(version) => return Ok(version),
            }
        }

        let content = resp.text().await.ok();
        return Err(Error::CommonError(format!(
            "Update fail with resp code {} content {:?}",
            code, content
        )));
    }

    pub async fn Delete(
        &self,
        token: &str,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<i64> {
        let client = self.Client();
        let url = format!(
            "{}/object/{objType}/{tenant}/{namespace}/{name}/",
            &self.url
        );
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let resp = client.delete(&url).headers(headers).send().await?;
        let code = resp.status().as_u16();
        let content = resp.text().await?;
        println!("Delete response is {:?} content {}", code, content);
        if code == StatusCode::OK {
            let res = content;
            match res.parse::<i64>() {
                Err(e) => {
                    return Err(Error::CommonError(format!(
                        "can't parse res with error {:?}",
                        e
                    )))
                }
                Ok(version) => return Ok(version),
            }
        }

        return Err(Error::CommonError(format!(
            "Delete fail with resp http code {:?}",
            code
        )));
    }

    pub async fn Grant(&self, token: &str, grant: &Grant) -> Result<()> {
        let client = self.Client();
        let url = format!("{}/rbac/", &self.url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }
        let resp = client
            .post(&url)
            .headers(headers)
            .json(&grant)
            .send()
            .await?;
        let code = resp.status().as_u16();
        if code == StatusCode::OK {
            // let res = resp.text().await?;
            println!("Grant success for {:#?}", grant);
            return Ok(());
        }

        let content = resp.text().await.ok();
        return Err(Error::CommonError(format!(
            "Grant fail with resp code {} content {:?}",
            code, content
        )));
    }

    pub async fn Revoke(&self, token: &str, grant: &Grant) -> Result<()> {
        let client = self.Client();
        let url = format!("{}/rbac/", &self.url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }
        let resp = client
            .delete(&url)
            .headers(headers)
            .json(&grant)
            .send()
            .await?;

        let code = resp.status().as_u16();
        if code == StatusCode::OK {
            return Ok(());
        }

        let content = resp.text().await.ok();
        return Err(Error::CommonError(format!(
            "Revoke fail with resp code {} content {:?}",
            code, content
        )));
    }

    pub async fn TenantUsers(
        &self,
        token: &str,
        role: &UserRole,
        tenant: &str,
    ) -> Result<Vec<String>> {
        let client = self.Client();
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let url = format!(
            "{}/rbac/tenantusers/{}/{}/",
            &self.url,
            role.String(),
            tenant
        );
        let body = client
            .get(&url)
            .headers(headers)
            .send()
            .await?
            .text()
            .await?;
        let obj = serde_json::from_str(&body)?;
        return Ok(obj);
    }

    pub async fn NamespaceUsers(
        &self,
        token: &str,
        role: &UserRole,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<String>> {
        let client = self.Client();
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let url = format!(
            "{}/rbac/namespaceusers/{}/{}/{}/",
            &self.url,
            role.String(),
            tenant,
            namespace
        );
        let body = client
            .get(&url)
            .headers(headers)
            .send()
            .await?
            .text()
            .await?;
        let obj = serde_json::from_str(&body)?;
        return Ok(obj);
    }

    pub async fn Roles(&self, token: &str) -> Result<Vec<RoleBinding>> {
        let client = self.Client();
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let url = format!("{}/rbac/roles/", &self.url);
        let body = client
            .get(&url)
            .headers(headers)
            .send()
            .await?
            .text()
            .await?;
        let obj = serde_json::from_str(&body)?;
        return Ok(obj);
    }

    pub async fn CreateApikey(
        &self,
        token: &str,
        req: &ApikeyCreateRequest,
    ) -> Result<ApikeyCreateResponse> {
        let client = self.Client();
        let url = format!("{}/apikey/", &self.url);
        println!("CreateApikey url {}", &url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let resp = client.put(&url).headers(headers).json(req).send().await?;
        let code = resp.status().as_u16();
        let content = resp.text().await?;
        if code == StatusCode::OK {
            let created = serde_json::from_str(&content)?;
            return Ok(created);
        }

        return Err(Error::CommonError(format!(
            "Create fail with resp {}",
            content
        )));
    }

    pub async fn DeleteApikey(
        &self,
        token: &str,
        req: &ApikeyDeleteRequest,
    ) -> Result<String> {
        let client = self.Client();
        let url = format!("{}/apikey/", &self.url);
        println!("DeleteApikey url {}", &url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let resp = client
            .delete(&url)
            .headers(headers)
            .json(req)
            .send()
            .await?;
        let code = resp.status().as_u16();
        let content = resp.text().await?;
        println!("DeleteApikey code {:?} content {}", code, &content);
        if code == StatusCode::OK {
            return Ok(content);
        }

        return Err(Error::CommonError(format!(
            "Delete fail with resp {}",
            content
        )));
    }

    pub async fn ListApikeys(&self, token: &str) -> Result<Vec<Apikey>> {
        let client = self.Client();
        let url = format!("{}/apikey/", &self.url);
        println!("GetApikey url {}", &url);
        let mut headers = HeaderMap::new();
        if token.len() > 0 {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }

        let resp = client.get(&url).headers(headers).send().await?;
        let code = resp.status().as_u16();
        if code == StatusCode::OK {
            let res = resp.text().await?;
            let apikeys = serde_json::from_str(&res)?;
            return Ok(apikeys);
        }

        let content = resp.text().await?;
        return Err(Error::CommonError(format!(
            "Create fail with resp {}",
            content
        )));
    }
}
