// Copyright 2023 Zinc Labs Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::io::{Error, ErrorKind};

use crate::{
    common::{
        infra::config::USERS_RUM_TOKEN,
        meta::{
            organization::{
                IngestionPasscode, IngestionTokensContainer, OrgSummary, Organization,
                RumIngestionToken,
            },
            user::UserOrg,
        },
        utils::{auth::is_root_user, rand::generate_random_string},
    },
    service::{db, stream::get_streams},
};

#[tracing::instrument]
pub async fn get_summary(org_id: &str) -> OrgSummary {
    let streams = get_streams(org_id, None, false).await;
    let functions = db::functions::list(org_id).await.unwrap();
    let alerts = db::alerts::list(org_id, None, None).await.unwrap();
    OrgSummary {
        streams,
        functions,
        alerts,
    }
}

#[tracing::instrument]
pub async fn get_passcode(
    org_id: Option<&str>,
    user_id: &str,
) -> Result<IngestionPasscode, anyhow::Error> {
    let Ok(Some(user)) = db::user::get(org_id, user_id).await else {
        return Err(anyhow::Error::msg("User not found"));
    };
    Ok(IngestionPasscode {
        user: user.email,
        passcode: user.token,
    })
}

#[tracing::instrument]
pub async fn get_rum_token(
    org_id: Option<&str>,
    user_id: &str,
) -> Result<RumIngestionToken, anyhow::Error> {
    let Ok(Some(user)) = db::user::get(org_id, user_id).await else {
        return Err(anyhow::Error::msg("User not found"));
    };
    Ok(RumIngestionToken {
        user: user.email,
        rum_token: user.rum_token,
    })
}

#[tracing::instrument]
pub async fn update_rum_token(
    org_id: Option<&str>,
    user_id: &str,
) -> Result<RumIngestionToken, anyhow::Error> {
    let is_rum_update = true;
    match update_passcode_inner(org_id, user_id, is_rum_update).await {
        Ok(IngestionTokensContainer::RumToken(response)) => Ok(response),
        _ => Err(anyhow::Error::msg("User not found")),
    }
}

#[tracing::instrument]
pub async fn update_passcode(
    org_id: Option<&str>,
    user_id: &str,
) -> Result<IngestionPasscode, anyhow::Error> {
    let is_rum_update = false;
    match update_passcode_inner(org_id, user_id, is_rum_update).await {
        Ok(IngestionTokensContainer::Passcode(response)) => Ok(response),
        _ => Err(anyhow::Error::msg("User not found")),
    }
}

#[tracing::instrument]
async fn update_passcode_inner(
    org_id: Option<&str>,
    user_id: &str,
    is_rum_update: bool,
) -> Result<IngestionTokensContainer, anyhow::Error> {
    let mut local_org_id = "dummy";
    let Ok(mut db_user) = db::user::get_db_user(user_id).await else {
        return Err(anyhow::Error::msg("User not found"));
    };

    if org_id.is_some() {
        local_org_id = org_id.unwrap();
    }
    let token = generate_random_string(16);
    let rum_token = format!("rum{}", generate_random_string(16));

    let updated_org = |existing_org: &UserOrg| {
        if is_rum_update {
            UserOrg {
                rum_token: Some(rum_token.clone()),
                ..existing_org.clone()
            }
        } else {
            UserOrg {
                token: token.clone(),
                ..existing_org.clone()
            }
        }
    };

    let mut orgs = db_user.clone().organizations;
    let new_orgs = if !is_root_user(user_id) {
        let mut existing_org = orgs.clone();

        // Find the org which we need to update
        existing_org.retain(|org| org.name.eq(&local_org_id));

        // Filter out the org which needs to be updated, so that we can modify and
        // insert it back.
        orgs.retain(|org| !org.name.eq(&local_org_id));

        // Invalidate the local cache
        let org_to_update = &existing_org[0];
        USERS_RUM_TOKEN.clone().remove(&format!(
            "{}/{}",
            org_to_update.name,
            org_to_update.rum_token.as_deref().unwrap_or_default()
        ));

        let updated_org = updated_org(&existing_org[0]);
        orgs.push(updated_org);
        orgs
    } else {
        // This is a root-user, so pick up the first/default org.
        let existing_org = orgs.first().unwrap().clone();

        let org_to_update = &existing_org;
        USERS_RUM_TOKEN.clone().remove(&format!(
            "{}/{}",
            org_to_update.name,
            org_to_update.rum_token.as_deref().unwrap_or_default()
        ));

        let updated_org = updated_org(&existing_org);
        vec![updated_org]
    };

    db_user.organizations = new_orgs;
    let _ = db::user::set(db_user.clone()).await;

    let ret = if is_rum_update {
        IngestionTokensContainer::RumToken(RumIngestionToken {
            user: db_user.email,
            rum_token: Some(rum_token),
        })
    } else {
        IngestionTokensContainer::Passcode(IngestionPasscode {
            user: db_user.email,
            passcode: token,
        })
    };
    Ok(ret)
}

#[tracing::instrument]
pub async fn create_org(org: &Organization) -> Result<Organization, Error> {
    match db::organization::set(org).await {
        Ok(_) => Ok(org.clone()),
        Err(e) => {
            log::error!("Error creating org: {}", e);
            Err(Error::new(
                ErrorKind::Other,
                format!("Error creating org: {}", e),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{common::meta::user::UserRequest, service::users};

    #[actix_web::test]
    async fn test_organization() {
        let org_id = "default";
        let user_id = "userone@example.com";
        let init_user = "root@example.com";
        // let passcode = "samplePassCode";
        let resp = users::post_user(
            org_id,
            UserRequest {
                email: user_id.to_string(),
                password: "pass".to_string(),
                role: crate::common::meta::user::UserRole::Admin,
                first_name: "admin".to_owned(),
                last_name: "".to_owned(),
                is_external: false,
            },
            init_user,
        )
        .await;
        assert!(resp.is_ok());
        assert!(resp.unwrap().status().is_success());

        let resp = get_passcode(Some(org_id), user_id).await.unwrap();
        let passcode = resp.passcode.clone();
        assert!(!resp.passcode.is_empty());

        let resp = update_passcode(Some(org_id), user_id).await.unwrap();
        assert_ne!(resp.passcode, passcode);
    }
}
