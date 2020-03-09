use std::{marker::PhantomData, sync::Arc, pin::Pin, task::{Poll, Context}};
use futures::{prelude::*, channel::mpsc::{UnboundedSender, UnboundedReceiver}};
use sp_runtime::traits::Block as BlockT;
use sc_network::PeerId;
use sc_network_gossip::{
	Validator as ValidatorT, ValidatorContext, GossipEngine, Network as GossipNetwork,
	ValidationResult,
};
use sp_consensus_sassafras::AuthorityId;

pub use sp_consensus_sassafras::SASSAFRAS_ENGINE_ID;
pub const SASSAFRAS_PROTOCOL_NAME: &[u8] = b"/paritytech/sassafras/1";

pub struct GossipValidator<Block: BlockT> {
	_marker: PhantomData<Block>,
}

impl<Block: BlockT> ValidatorT<Block> for GossipValidator<Block> {
	fn validate(
		&self,
		context: &mut dyn ValidatorContext<Block>,
		sender: &PeerId,
		data: &[u8]
	) -> ValidationResult<Block::Hash> {
		unimplemented!()
	}
}

pub struct NetworkBridge<Block: BlockT, N> {
	service: N,
	gossip_engine: GossipEngine<Block>,
	validator: Arc<GossipValidator<Block>>,
	local_out_proofs: UnboundedReceiver<(AuthorityId, [u8; 32], Vec<u8>)>,
	remote_in_proofs: UnboundedSender<(AuthorityId, [u8; 32], Vec<u8>)>,
}

impl<Block: BlockT, N> NetworkBridge<Block, N> where
	N: GossipNetwork<Block> + Clone + Send + 'static,
{
	pub fn new(
		service: N,
		local_out_proofs: UnboundedReceiver<(AuthorityId, [u8; 32], Vec<u8>)>,
		remote_in_proofs: UnboundedSender<(AuthorityId, [u8; 32], Vec<u8>)>,
	) -> Self {
		let validator = Arc::new(GossipValidator {
			_marker: PhantomData,
		});

		let gossip_engine = GossipEngine::new(
			service.clone(),
			SASSAFRAS_ENGINE_ID,
			SASSAFRAS_PROTOCOL_NAME,
			validator.clone(),
		);

		Self {
			service,
			gossip_engine,
			validator,
			local_out_proofs,
			remote_in_proofs,
		}
	}
}

impl<Block: BlockT, N: Unpin> Future for NetworkBridge<Block, N> {
	type Output = ();

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		self.gossip_engine.poll_unpin(cx)
	}
}
