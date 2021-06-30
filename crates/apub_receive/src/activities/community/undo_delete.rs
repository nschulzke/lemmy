use crate::{
  activities::community::{
    delete::DeleteCommunity,
    send_websocket_message,
    verify_is_community_mod,
  },
  inbox::new_inbox_routing::Activity,
};
use activitystreams::activity::kind::DeleteType;
use lemmy_api_common::blocking;
use lemmy_apub::{
  check_is_apub_id_valid,
  fetcher::{community::get_or_fetch_and_upsert_community, person::get_or_fetch_and_upsert_person},
  ActorType,
  CommunityType,
};
use lemmy_apub_lib::{verify_domains_match, ActivityHandler, PublicUrl};
use lemmy_db_queries::{source::community::Community_, ApubObject};
use lemmy_db_schema::source::community::Community;
use lemmy_utils::LemmyError;
use lemmy_websocket::{LemmyContext, UserOperationCrud};
use url::Url;

// We have two possibilities which need to be handled:
//     1. actor is remote mod, community id in object
//     2. actor is community, cc is followers collection
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoDeleteCommunity {
  to: PublicUrl,
  object: Activity<DeleteCommunity>,
  cc: [Url; 1],
  #[serde(rename = "type")]
  kind: DeleteType,
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for Activity<UndoDeleteCommunity> {
  type Actor = lemmy_apub::fetcher::Actor;

  async fn verify(&self, context: &LemmyContext) -> Result<(), LemmyError> {
    verify_domains_match(&self.actor, self.id_unchecked())?;
    let object = self.inner.object.inner.object.clone();
    let community = blocking(context.pool(), move |conn| {
      Community::read_from_apub_id(conn, &object.into())
    })
    .await?;
    // remote mod action on local community
    if let Ok(c) = community {
      verify_domains_match(&self.inner.object.inner.object, &self.inner.cc[0])?;
      check_is_apub_id_valid(&self.actor, false)?;
      verify_is_community_mod(self.actor.clone(), c.actor_id(), context).await?;
    }
    // community action sent to followers
    else {
      verify_domains_match(&self.actor, &self.inner.object.inner.object)?;
      verify_domains_match(&self.actor, &self.inner.cc[0])?;
    }
    self.inner.object.verify(context).await
  }

  async fn receive(
    &self,
    _actor: Self::Actor,
    context: &LemmyContext,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    let object = self.inner.object.inner.object.clone();
    let community = blocking(context.pool(), move |conn| {
      Community::read_from_apub_id(conn, &object.into())
    })
    .await?;
    let community_id = match community {
      Ok(c) => {
        // remote mod sent undo to local community, forward it to followers
        let actor = get_or_fetch_and_upsert_person(&self.actor, context, request_counter).await?;
        c.send_delete(actor, context).await?;
        c.id
      }
      Err(_) => {
        // refetch the remote community
        let community = get_or_fetch_and_upsert_community(
          &self.inner.object.inner.object,
          context,
          request_counter,
        )
        .await?;
        community.id
      }
    };
    let restored_community = blocking(context.pool(), move |conn| {
      Community::update_deleted(conn, community_id, false)
    })
    .await??;

    send_websocket_message(
      restored_community.id,
      UserOperationCrud::EditCommunity,
      context,
    )
    .await
  }
}