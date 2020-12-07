#![feature(prelude_import)]
//! Various implementation for `ElectionProvider`.
//!
//! Two main election providers are implemented in this crate.
//!
//! 1.  [`onchain`]: A `struct` that perform the election onchain (i.e. in the fly). This type is
//!     likely to be expensive for most chains and damage the block time. Only use when you are sure
//!     that the inputs are bounded and small enough.
//! 2. [`two_phase`]: An individual `pallet` that performs the election in two phases, signed and
//!    unsigned. Needless to say, the pallet needs to be included in the final runtime.
#[prelude_import]
use std::prelude::v1::*;
#[macro_use]
extern crate std;
/// The onchain module.
pub mod onchain {
    use sp_arithmetic::PerThing;
    use sp_election_providers::ElectionProvider;
    use sp_npos_elections::{
        ElectionResult, ExtendedBalance, IdentifierT, PerThing128, Supports, VoteWeight,
    };
    use sp_runtime::RuntimeDebug;
    use sp_std::{collections::btree_map::BTreeMap, prelude::*};
    /// Errors of the on-chain election.
    pub enum Error {
        /// An internal error in the NPoS elections crate.
        NposElections(sp_npos_elections::Error),
    }
    impl core::fmt::Debug for Error {
        fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
            match self {
                Self::NposElections(ref a0) => {
                    fmt.debug_tuple("Error::NposElections").field(a0).finish()
                }
                _ => Ok(()),
            }
        }
    }
    impl ::core::marker::StructuralEq for Error {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl ::core::cmp::Eq for Error {
        #[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {
                let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
            }
        }
    }
    impl ::core::marker::StructuralPartialEq for Error {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl ::core::cmp::PartialEq for Error {
        #[inline]
        fn eq(&self, other: &Error) -> bool {
            match (&*self, &*other) {
                (&Error::NposElections(ref __self_0), &Error::NposElections(ref __arg_1_0)) => {
                    (*__self_0) == (*__arg_1_0)
                }
            }
        }
        #[inline]
        fn ne(&self, other: &Error) -> bool {
            match (&*self, &*other) {
                (&Error::NposElections(ref __self_0), &Error::NposElections(ref __arg_1_0)) => {
                    (*__self_0) != (*__arg_1_0)
                }
            }
        }
    }
    impl From<sp_npos_elections::Error> for Error {
        fn from(e: sp_npos_elections::Error) -> Self {
            Error::NposElections(e)
        }
    }
    /// A simple on-chian implementation of the election provider trait.
    ///
    /// This will accept voting data on the fly and produce the results immediately.
    ///
    /// ### Warning
    ///
    /// This can be very expensive to run frequently on-chain. Use with care.
    pub struct OnChainSequentialPhragmen;
    impl<AccountId: IdentifierT> ElectionProvider<AccountId> for OnChainSequentialPhragmen {
        type Error = Error;
        const NEEDS_ELECT_DATA: bool = true;
        fn elect<P: PerThing128>(
            to_elect: usize,
            targets: Vec<AccountId>,
            voters: Vec<(AccountId, VoteWeight, Vec<AccountId>)>,
        ) -> Result<Supports<AccountId>, Self::Error>
        where
            ExtendedBalance: From<<P as PerThing>::Inner>,
        {
            let mut stake_map: BTreeMap<AccountId, VoteWeight> = BTreeMap::new();
            voters.iter().for_each(|(v, s, _)| {
                stake_map.insert(v.clone(), *s);
            });
            let stake_of = Box::new(|w: &AccountId| -> VoteWeight {
                stake_map.get(w).cloned().unwrap_or_default()
            });
            sp_npos_elections::seq_phragmen::<_, P>(to_elect, targets, voters, None)
                .and_then(|e| {
                    let ElectionResult {
                        winners,
                        assignments,
                    } = e;
                    let staked = sp_npos_elections::assignment_ratio_to_staked_normalized(
                        assignments,
                        &stake_of,
                    )?;
                    let winners = sp_npos_elections::to_without_backing(winners);
                    sp_npos_elections::to_supports(&winners, &staked)
                })
                .map_err(From::from)
        }
        fn ongoing() -> bool {
            false
        }
    }
}
/// The two-phase module.
pub mod two_phase {
	//! # Two phase election provider pallet.
	//!
	//! As the name suggests, this election-provider has two distinct phases (see [`Phase`]), signed and
	//! unsigned.
	//!
	//! ## Phases
	//!
	//! The timeline of pallet is as follows. At each block,
	//! [`ElectionDataProvider::next_election_prediction`] is used to estimate the time remaining to the
	//! next call to `elect`. Based on this, a phase is chosen. The timeline is as follows.
	//!
	//! ```ignore
	//!                                                                    elect()
	//!                 +   <--T::SignedPhase-->  +  <--T::UnsignedPhase-->   +
	//!   +-------------------------------------------------------------------+
	//!    Phase::Off   +       Phase::Signed     +      Phase::Unsigned      +
	//!
	//! Note that the unsigned phase starts `T::UnsignedPhase` blocks before the
	//! `next_election_prediction`, but only ends when a call to `ElectionProvider::elect` happens.
	//!
	//! ```
	//! ### Signed Phase
	//!
	//!	In the signed phase, solutions (of type [`RawSolution`]) are submitted and queued on chain. A
	//! deposit is reserved, based on the size of the solution, for the cost of keeping this solution
	//! on-chain for a number of blocks. A maximum of [`Config::MaxSignedSubmissions`] solutions are
	//! stored. The queue is always sorted based on score (worse to best).
	//!
	//! Upon arrival of a new solution:
	//!
	//! 1. If the queue is not full, it is stored in the appropriate index.
	//! 2. If the queue is full but the submitted solution is better than one of the queued ones, the
	//!    worse solution is discarded (TODO: must return the bond here) and the new solution is stored
	//!    in the correct index.
	//! 3. If the queue is full and the solution is not an improvement compared to any of the queued
	//!    ones, it is instantly rejected and no additional bond is reserved.
	//!
	//! A signed solution cannot be reversed, taken back, updated, or retracted. In other words, the
	//! origin can not bail out in any way.
	//!
	//! Upon the end of the signed phase, the solutions are examined from worse to best (i.e. `pop()`ed
	//! until drained). Each solution undergoes an expensive [`Module::feasibility_check`], which ensure
	//! the score claimed by this score was correct, among other checks. At each step, if the current
	//! best solution passes the feasibility check, it is considered to be the best one. The sender
	//! of the origin is rewarded, and the rest of the queued solutions get their deposit back, without
	//! being checked.
	//!
	//! The following example covers all of the cases at the end of the signed phase:
	//!
	//! ```ignore
	//! Queue
	//! +-------------------------------+
	//! |Solution(score=20, valid=false)| +-->  Slashed
	//! +-------------------------------+
	//! |Solution(score=15, valid=true )| +-->  Rewarded
	//! +-------------------------------+
	//! |Solution(score=10, valid=true )| +-->  Discarded
	//! +-------------------------------+
	//! |Solution(score=05, valid=false)| +-->  Discarded
	//! +-------------------------------+
	//! |             None              |
	//! +-------------------------------+
	//! ```
	//!
	//! TODO: what if length of some phase is zero?
	//!
	//! Note that both of the bottom solutions end up being discarded and get their deposit back,
	//! despite one of them being invalid.
	//!
	//! ## Unsigned Phase
	//!
	//! If signed phase ends with a good solution, then the unsigned phase will be `active`
	//! ([`Phase::Unsigned(true)`]), else the unsigned phase will be `passive`.
	//!
	//! TODO
	//!
	//! ### Fallback
	//!
	//! If we reach the end of both phases (i.e. call to `ElectionProvider::elect` happens) and no good
	//! solution is queued, then we fallback to an on-chain election. The on-chain election is slow, and
	//! contains to balancing or reduction post-processing.
	//!
	//! ## Correct Submission
	//!
	//! TODO
	//!
	//! ## Accuracy
	//!
	//! TODO
	//!
	use crate::onchain::OnChainSequentialPhragmen;
	use codec::{Decode, Encode, HasCompact};
	use frame_support::{
		decl_event, decl_module, decl_storage,
		dispatch::DispatchResultWithPostInfo,
		ensure,
		traits::{Currency, Get, OnUnbalanced, ReservableCurrency},
		weights::Weight,
	};
	use frame_system::{ensure_none, ensure_signed, offchain::SendTransactionTypes};
	use sp_election_providers::{ElectionDataProvider, ElectionProvider};
	use sp_npos_elections::{
		assignment_ratio_to_staked_normalized, is_score_better, Assignment, CompactSolution,
		ElectionScore, EvaluateSupport, ExtendedBalance, PerThing128, Supports, VoteWeight,
	};
	use sp_runtime::{
		traits::Zero, transaction_validity::TransactionPriority, InnerOf, PerThing, Perbill,
		RuntimeDebug,
	};
	use sp_std::prelude::*;
	#[macro_use]
	pub(crate) mod macros {
		//! Some helper macros for this crate.
	}
	pub mod signed {
		//! The signed phase implementation.
		use crate::two_phase::*;
		use codec::Encode;
		use sp_arithmetic::traits::SaturatedConversion;
		use sp_npos_elections::is_score_better;
		use sp_runtime::Perbill;
		impl<T: Config> Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			/// Start the signed phase.
			///
			/// Upon calling this, auxillary data for election is stored and signed solutions will be
			/// accepted.
			///
			/// The signed phase must always start before the unsigned phase.
			pub fn start_signed_phase() {
				let targets = T::ElectionDataProvider::targets();
				let voters = T::ElectionDataProvider::voters();
				let desired_targets = T::ElectionDataProvider::desired_targets();
				<Snapshot<T>>::put(RoundSnapshot {
					voters,
					targets,
					desired_targets,
				});
			}
			/// Finish the singed phase. Process the signed submissions from best to worse until a valid one
			/// is found, rewarding the best oen and slashing the invalid ones along the way.
			///
			/// Returns true if we have a good solution in the signed phase.
			///
			/// This drains the [`SignedSubmissions`], potentially storing the best valid one in
			/// [`QueuedSolution`].
			pub fn finalize_signed_phase() -> bool {
				let mut all_submission: Vec<SignedSubmission<_, _, _>> =
					<SignedSubmissions<T>>::take();
				let mut found_solution = false;
				while let Some(best) = all_submission.pop() {
					let SignedSubmission {
                        solution,
                        who,
                        deposit,
                        reward,
                    } = best;
                    match Self::feasibility_check(solution, ElectionCompute::Signed) {
                        Ok(ready_solution) => {
                            <QueuedSolution<T>>::put(ready_solution);
                            let _remaining = T::Currency::unreserve(&who, deposit);
                            if true {
                                if !_remaining.is_zero() {
                                    {
                                        ::std::rt::begin_panic(
                                            "assertion failed: _remaining.is_zero()",
                                        )
                                    }
                                };
                            };
                            let positive_imbalance = T::Currency::deposit_creating(&who, reward);
                            T::RewardHandler::on_unbalanced(positive_imbalance);
                            found_solution = true;
                            break;
                        }
                        Err(_) => {
                            let (negative_imbalance, _remaining) =
                                T::Currency::slash_reserved(&who, deposit);
                            if true {
                                if !_remaining.is_zero() {
                                    {
                                        ::std::rt::begin_panic(
                                            "assertion failed: _remaining.is_zero()",
                                        )
                                    }
                                };
                            };
                            T::SlashHandler::on_unbalanced(negative_imbalance);
                        }
                    }
                }
                all_submission.into_iter().for_each(|not_processed| {
                    let SignedSubmission { who, deposit, .. } = not_processed;
                    let _remaining = T::Currency::unreserve(&who, deposit);
                    if true {
                        if !_remaining.is_zero() {
                            {
                                ::std::rt::begin_panic("assertion failed: _remaining.is_zero()")
                            }
                        };
                    };
                });
				found_solution
			}
			/// Find a proper position in the queue for the signed queue, whilst maintaining the order of
			/// solution quality.
			///
			/// The length of the queue will always be kept less than or equal to `T::MaxSignedSubmissions`.
			pub fn insert_submission(
				who: &T::AccountId,
				queue: &mut Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>,
				solution: RawSolution<CompactOf<T>>,
			) -> Option<usize> {
				let outcome = queue
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(i, s)| {
                        if is_score_better::<Perbill>(
                            solution.score,
                            s.solution.score,
                            T::SolutionImprovementThreshold::get(),
                        ) {
                            Some(i + 1)
                        } else {
                            None
                        }
                    })
                    .or(Some(0))
                    .and_then(|at| {
                        if at == 0 && queue.len() as u32 >= T::MaxSignedSubmissions::get() {
                            None
                        } else {
                            let reward = Self::reward_for(&solution);
                            let deposit = Self::deposit_for(&solution);
                            let submission = SignedSubmission {
                                who: who.clone(),
                                deposit,
                                reward,
                                solution,
                            };
                            queue.insert(at, submission);
                            if queue.len() as u32 > T::MaxSignedSubmissions::get() {
                                queue.remove(0);
                                Some(at - 1)
                            } else {
                                Some(at)
                            }
                        }
                    });
                if true {
                    if !(queue.len() as u32 <= T::MaxSignedSubmissions::get()) {
                        {
                            :: std :: rt :: begin_panic ( "assertion failed: queue.len() as u32 <= T::MaxSignedSubmissions::get()" )
                        }
                    };
                };
				outcome
			}
			/// Collect sufficient deposit to store this solution this chain.
			///
			/// The deposit is composed of 3 main elements:
			///
			/// 1. base deposit, fixed for all submissions.
			/// 2. a per-byte deposit, for renting the state usage.
			/// 3. a per-weight deposit, for the potential weight usage in an upcoming on_initialize
			pub fn deposit_for(solution: &RawSolution<CompactOf<T>>) -> BalanceOf<T> {
				let encoded_len: BalanceOf<T> = solution.using_encoded(|e| e.len() as u32).into();
				let feasibility_weight = T::WeightInfo::feasibility_check();
				let len_deposit = T::SignedDepositByte::get() * encoded_len;
				let weight_deposit =
					T::SignedDepositWeight::get() * feasibility_weight.saturated_into();
				T::SignedDepositBase::get() + len_deposit + weight_deposit
			}
			/// The reward for this solution, if successfully chosen as the best one at the end of the
			/// signed phase.
			pub fn reward_for(solution: &RawSolution<CompactOf<T>>) -> BalanceOf<T> {
				T::SignedRewardBase::get()
					+ T::SignedRewardFactor::get()
						* solution.score[0].saturated_into::<BalanceOf<T>>()
			}
		}
	}
	pub mod unsigned {
		//! The unsigned phase implementation.
		use crate::two_phase::*;
		use frame_support::{dispatch::DispatchResult, unsigned::ValidateUnsigned};
		use frame_system::offchain::SubmitTransaction;
		use sp_npos_elections::{seq_phragmen, CompactSolution, ElectionResult};
		use sp_runtime::{
			offchain::storage::StorageValueRef,
			traits::TrailingZeroInput,
			transaction_validity::{
				InvalidTransaction, TransactionSource, TransactionValidity,
				TransactionValidityError, ValidTransaction,
			},
			SaturatedConversion,
		};
		use sp_std::cmp::Ordering;
		/// Storage key used to store the persistent offchain worker status.
		pub(crate) const OFFCHAIN_HEAD_DB: &[u8] = b"parity/unsigned-election/";
		/// The repeat threshold of the offchain worker. This means we won't run the offchain worker twice
		/// within a window of 5 blocks.
		pub(crate) const OFFCHAIN_REPEAT: u32 = 5;
		/// Default number of blocks for which the unsigned transaction should stay in the pool
		pub(crate) const DEFAULT_LONGEVITY: u64 = 25;
		impl<T: Config> Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			/// Min a new npos solution.
			pub fn mine_solution(iters: usize) -> Result<RawSolution<CompactOf<T>>, Error> {
				let desired_targets = Self::desired_targets() as usize;
				let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
				let targets = Self::snapshot_targets().ok_or(Error::SnapshotUnAvailable)?;
				seq_phragmen::<_, CompactAccuracyOf<T>>(
					desired_targets,
					targets,
					voters,
					Some((iters, 0)),
				)
				.map_err(Into::into)
				.and_then(Self::prepare_election_result)
			}
			/// Convert a raw solution from [`sp_npos_elections::ElectionResult`] to [`RawSolution`], which
			/// is ready to be submitted to the chain.
			///
			/// Will always reduce the solution as well.
			pub fn prepare_election_result(
				election_result: ElectionResult<T::AccountId, CompactAccuracyOf<T>>,
			) -> Result<RawSolution<CompactOf<T>>, Error> {
				let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
				let targets = Self::snapshot_targets().ok_or(Error::SnapshotUnAvailable)?;
				let voter_index =
					|who: &T::AccountId| -> Option<crate::two_phase::CompactVoterIndexOf<T>> {
						voters . iter ( ) . position ( | ( x , _ , _ ) | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactVoterIndexOf < T > > > :: try_into ( i ) . ok ( ) )
					};
				let target_index =
					|who: &T::AccountId| -> Option<crate::two_phase::CompactTargetIndexOf<T>> {
						targets . iter ( ) . position ( | x | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactTargetIndexOf < T > > > :: try_into ( i ) . ok ( ) )
					};
				let voter_at =
					|i: crate::two_phase::CompactVoterIndexOf<T>| -> Option<T::AccountId> {
						< crate :: two_phase :: CompactVoterIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | voters . get ( i ) . map ( | ( x , _ , _ ) | x ) . cloned ( ) )
					};
				let target_at =
					|i: crate::two_phase::CompactTargetIndexOf<T>| -> Option<T::AccountId> {
                        < crate :: two_phase :: CompactTargetIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | targets . get ( i ) . cloned ( ) )
                    };
                let stake_of = |who: &T::AccountId| -> crate::VoteWeight {
                    voters
                        .iter()
                        .find(|(x, _, _)| x == who)
                        .map(|(_, x, _)| *x)
                        .unwrap_or_default()
                };
                let ElectionResult {
                    assignments,
                    winners,
                } = election_result;
                let mut staked = sp_npos_elections::assignment_ratio_to_staked_normalized(
                    assignments,
                    &stake_of,
                )
                .map_err::<Error, _>(Into::into)?;
				sp_npos_elections::reduce(&mut staked);
				let ratio = sp_npos_elections::assignment_staked_to_ratio_normalized(staked)?;
				let compact = <CompactOf<T>>::from_assignment(ratio, &voter_index, &target_index)?;
				let maximum_allowed_voters =
					Self::maximum_compact_len::<T::WeightInfo>(0, Default::default(), 0);
				let compact = Self::trim_compact(compact.len() as u32, compact, &voter_index)?;
				let winners = sp_npos_elections::to_without_backing(winners);
				let score = compact
					.clone()
					.score(&winners, stake_of, voter_at, target_at)?;
				Ok(RawSolution { compact, score })
			}
			/// Get a random number of iterations to run the balancing in the OCW.
			///
			/// Uses the offchain seed to generate a random number, maxed with `T::UnsignedMaxIterations`.
			pub fn get_balancing_iters() -> usize {
				match T::UnsignedMaxIterations::get() {
					0 => 0,
					max @ _ => {
						let seed = sp_io::offchain::random_seed();
						let random = <u32>::decode(&mut TrailingZeroInput::new(seed.as_ref()))
							.expect("input is padded with zeroes; qed")
							% max.saturating_add(1);
						random as usize
					}
				}
			}
			/// Greedily reduce the size of the a solution to fit into the block, w.r.t. weight.
			///
			/// The weight of the solution is foremost a function of the number of voters (i.e.
			/// `compact.len()`). Aside from this, the other components of the weight are invariant. The
			/// number of winners shall not be changed (otherwise the solution is invalid) and the
			/// `ElectionSize` is merely a representation of the total number of stakers.
			///
			/// Thus, we reside to stripping away some voters. This means only changing the `compact`
			/// struct.
			///
			/// Note that the solution is already computed, and the winners are elected based on the merit
			/// of teh entire stake in the system. Nonetheless, some of the voters will be removed further
			/// down the line.
			///
			/// Indeed, the score must be computed **after** this step. If this step reduces the score too
			/// much, then the solution will be discarded.
			pub fn trim_compact<FN>(
				maximum_allowed_voters: u32,
				mut compact: CompactOf<T>,
				nominator_index: FN,
			) -> Result<CompactOf<T>, Error>
			where
				for<'r> FN: Fn(&'r T::AccountId) -> Option<CompactVoterIndexOf<T>>,
			{
				match compact.len().checked_sub(maximum_allowed_voters as usize) {
					Some(to_remove) if to_remove > 0 => {
						let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
						let mut voters_sorted = voters
							.into_iter()
							.map(|(who, stake, _)| (who.clone(), stake))
							.collect::<Vec<_>>();
						voters_sorted.sort_by_key(|(_, y)| *y);
						let mut removed = 0;
						for (maybe_index, _stake) in voters_sorted
							.iter()
							.map(|(who, stake)| (nominator_index(&who), stake))
						{
							let index = maybe_index.ok_or(Error::SnapshotUnAvailable)?;
							if compact.remove_voter(index) {
								removed += 1
							}
							if removed >= to_remove {
                                break;
                            }
						}
						Ok(compact)
					}
					_ => Ok(compact),
				}
			}
			/// Find the maximum `len` that a compact can have in order to fit into the block weight.
			///
			/// This only returns a value between zero and `size.nominators`.
			pub fn maximum_compact_len<W: WeightInfo>(
				_winners_len: u32,
				witness: WitnessData,
				max_weight: Weight,
			) -> u32 {
				if witness.voters < 1 {
					return witness.voters;
				}
				let max_voters = witness.voters.max(1);
				let mut voters = max_voters;
				let weight_with = |_voters: u32| -> Weight { W::submit_unsigned() };
				let next_voters =
					|current_weight: Weight, voters: u32, step: u32| -> Result<u32, ()> {
						match current_weight.cmp(&max_weight) {
                            Ordering::Less => {
                                let next_voters = voters.checked_add(step);
                                match next_voters {
                                    Some(voters) if voters < max_voters => Ok(voters),
                                    _ => Err(()),
                                }
                            }
                            Ordering::Greater => voters.checked_sub(step).ok_or(()),
                            Ordering::Equal => Ok(voters),
                        }
                    };
                let mut step = voters / 2;
                let mut current_weight = weight_with(voters);
                while step > 0 {
                    match next_voters(current_weight, voters, step) {
                        Ok(next) if next != voters => {
                            voters = next;
                        }
                        Err(()) => {
                            break;
                        }
                        Ok(next) => return next,
                    }
                    step = step / 2;
                    current_weight = weight_with(voters);
                }
                while voters + 1 <= max_voters && weight_with(voters + 1) < max_weight {
                    voters += 1;
                }
                while voters.checked_sub(1).is_some() && weight_with(voters) > max_weight {
                    voters -= 1;
                }
                if true {
                    if !(weight_with(voters.min(witness.voters)) <= max_weight) {
                        {
                            ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                                &["weight_with(", ") <= "],
                                &match (&voters.min(witness.voters), &max_weight) {
                                    (arg0, arg1) => [
                                        ::core::fmt::ArgumentV1::new(
                                            arg0,
                                            ::core::fmt::Display::fmt,
                                        ),
                                        ::core::fmt::ArgumentV1::new(
                                            arg1,
                                            ::core::fmt::Display::fmt,
                                        ),
                                    ],
                                },
                            ))
                        }
                    };
                };
				voters.min(witness.voters)
			}
			/// Checks if an execution of the offchain worker is permitted at the given block number, or not.
			///
			/// This essentially makes sure that we don't run on previous blocks in case of a re-org, and we
			/// don't run twice within a window of length [`OFFCHAIN_REPEAT`].
			///
			/// Returns `Ok(())` if offchain worker should happen, `Err(reason)` otherwise.
			pub(crate) fn set_check_offchain_execution_status(
				now: T::BlockNumber,
			) -> Result<(), &'static str> {
				let storage = StorageValueRef::persistent(&OFFCHAIN_HEAD_DB);
				let threshold = T::BlockNumber::from(OFFCHAIN_REPEAT);
				let mutate_stat = storage.mutate::<_, &'static str, _>(
					|maybe_head: Option<Option<T::BlockNumber>>| match maybe_head {
						Some(Some(head)) if now < head => Err("fork."),
						Some(Some(head)) if now >= head && now <= head + threshold => {
							Err("recently executed.")
						}
						Some(Some(head)) if now > head + threshold => Ok(now),
						_ => Ok(now),
					},
				);
				match mutate_stat {
					Ok(Ok(_)) => Ok(()),
					Ok(Err(_)) => Err("failed to write to offchain db."),
					Err(why) => Err(why),
				}
			}
			/// Mine a new solution, and submit it back to the chian as an unsigned transaction.
			pub(crate) fn mine_and_submit() -> Result<(), Error> {
				let balancing = Self::get_balancing_iters();
				let raw_solution = Self::mine_solution(balancing)?;
				let call = Call::submit_unsigned(raw_solution).into();
				SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call)
					.map_err(|_| Error::PoolSubmissionFailed)
			}
			pub(crate) fn unsigned_pre_dispatch_checks(
				solution: &RawSolution<CompactOf<T>>,
			) -> DispatchResult {
				{
					if !Self::current_phase().is_unsigned_open() {
						{
							return Err(PalletError::<T>::EarlySubmission.into());
						};
					}
				};
				{
					if !Self::queued_solution().map_or(true, |q: ReadySolution<_>| {
						is_score_better::<Perbill>(
							solution.score,
							q.score,
							T::SolutionImprovementThreshold::get(),
						)
					}) {
						{
							return Err(PalletError::<T>::WeakSubmission.into());
						};
					}
				};
				Ok(())
			}
		}
		#[allow(deprecated)]
		impl<T: Config> ValidateUnsigned for Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			type Call = Call<T>;
			fn validate_unsigned(
				source: TransactionSource,
				call: &Self::Call,
			) -> TransactionValidity {
				if let Call::submit_unsigned(solution) = call {
                    match source {
                        TransactionSource::Local | TransactionSource::InBlock => {}
                        _ => {
                            return InvalidTransaction::Call.into();
                        }
                    }
                    if let Err(_why) = Self::pre_dispatch_checks(solution) {
                        return InvalidTransaction::Custom(99).into();
                    }
                    ValidTransaction::with_tag_prefix("OffchainElection")
                        .priority(
                            T::UnsignedPriority::get()
                                .saturating_add(solution.score[0].saturated_into()),
                        )
                        .longevity(DEFAULT_LONGEVITY)
                        .propagate(false)
                        .build()
                } else {
                    InvalidTransaction::Call.into()
                }
            }
            fn pre_dispatch(call: &Self::Call) -> Result<(), TransactionValidityError> {
                if let Call::submit_unsigned(solution) = call {
                    Self::pre_dispatch_checks(solution)
                        .map_err(|_| InvalidTransaction::Custom(99).into())
                } else {
                    Err(InvalidTransaction::Call.into())
                }
			}
		}
	}
	/// The compact solution type used by this crate. This is provided from the [`ElectionDataProvider`]
	/// implementer.
	pub type CompactOf<T> = <<T as Config>::ElectionDataProvider as ElectionDataProvider<
		<T as frame_system::Config>::AccountId,
		<T as frame_system::Config>::BlockNumber,
	>>::CompactSolution;
	/// The voter index. Derived from [`CompactOf`].
	pub type CompactVoterIndexOf<T> = <CompactOf<T> as CompactSolution>::Voter;
	/// The target index. Derived from [`CompactOf`].
	pub type CompactTargetIndexOf<T> = <CompactOf<T> as CompactSolution>::Target;
	/// The accuracy of the election. Derived from [`CompactOf`].
	pub type CompactAccuracyOf<T> = <CompactOf<T> as CompactSolution>::VoteWeight;
	type BalanceOf<T> =
		<<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;
	type PositiveImbalanceOf<T> = <<T as Config>::Currency as Currency<
		<T as frame_system::Config>::AccountId,
	>>::PositiveImbalance;
	type NegativeImbalanceOf<T> = <<T as Config>::Currency as Currency<
		<T as frame_system::Config>::AccountId,
	>>::NegativeImbalance;
	/// Current phase of the pallet.
	pub enum Phase<Bn> {
		/// Nothing, the election is not happening.
		Off,
		/// Signed phase is open.
		Signed,
		/// Unsigned phase. First element is whether it is open or not, second the starting block
		/// number.
		Unsigned((bool, Bn)),
	}
	impl<Bn> ::core::marker::StructuralPartialEq for Phase<Bn> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::cmp::PartialEq> ::core::cmp::PartialEq for Phase<Bn> {
		#[inline]
		fn eq(&self, other: &Phase<Bn>) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
                    match (&*self, &*other) {
                        (&Phase::Unsigned(ref __self_0), &Phase::Unsigned(ref __arg_1_0)) => {
                            (*__self_0) == (*__arg_1_0)
                        }
                        _ => true,
                    }
                } else {
                    false
                }
            }
        }
        #[inline]
        fn ne(&self, other: &Phase<Bn>) -> bool {
            {
                let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
                let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
                if true && __self_vi == __arg_1_vi {
                    match (&*self, &*other) {
                        (&Phase::Unsigned(ref __self_0), &Phase::Unsigned(ref __arg_1_0)) => {
                            (*__self_0) != (*__arg_1_0)
                        }
                        _ => false,
                    }
                } else {
                    true
                }
			}
		}
	}
	impl<Bn> ::core::marker::StructuralEq for Phase<Bn> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::cmp::Eq> ::core::cmp::Eq for Phase<Bn> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<(bool, Bn)>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::clone::Clone> ::core::clone::Clone for Phase<Bn> {
		#[inline]
        fn clone(&self) -> Phase<Bn> {
            match (&*self,) {
                (&Phase::Off,) => Phase::Off,
                (&Phase::Signed,) => Phase::Signed,
                (&Phase::Unsigned(ref __self_0),) => {
                    Phase::Unsigned(::core::clone::Clone::clone(&(*__self_0)))
                }
            }
        }
	}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<Bn: ::core::marker::Copy> ::core::marker::Copy for Phase<Bn> {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<Bn> _parity_scale_codec::Encode for Phase<Bn>
		where
			Bn: _parity_scale_codec::Encode,
			(bool, Bn): _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				match *self {
					Phase::Off => {
						__codec_dest_edqy.push_byte(0usize as u8);
					}
					Phase::Signed => {
						__codec_dest_edqy.push_byte(1usize as u8);
					}
					Phase::Unsigned(ref aa) => {
						__codec_dest_edqy.push_byte(2usize as u8);
						__codec_dest_edqy.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<Bn> _parity_scale_codec::EncodeLike for Phase<Bn>
		where
			Bn: _parity_scale_codec::Encode,
			(bool, Bn): _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<Bn> _parity_scale_codec::Decode for Phase<Bn>
		where
			Bn: _parity_scale_codec::Decode,
			(bool, Bn): _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match __codec_input_edqy.read_byte()? {
					__codec_x_edqy if __codec_x_edqy == 0usize as u8 => Ok(Phase::Off),
					__codec_x_edqy if __codec_x_edqy == 1usize as u8 => Ok(Phase::Signed),
					__codec_x_edqy if __codec_x_edqy == 2usize as u8 => Ok(Phase::Unsigned({
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field Phase :: Unsigned.0".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					})),
					_ => Err("No such variant in enum Phase".into()),
				}
			}
		}
	};
	impl<Bn> core::fmt::Debug for Phase<Bn>
	where
		Bn: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
                Self::Off => fmt.debug_tuple("Phase::Off").finish(),
                Self::Signed => fmt.debug_tuple("Phase::Signed").finish(),
                Self::Unsigned(ref a0) => fmt.debug_tuple("Phase::Unsigned").field(a0).finish(),
                _ => Ok(()),
            }
		}
	}
	impl<Bn> Default for Phase<Bn> {
		fn default() -> Self {
			Phase::Off
		}
	}
    impl<Bn: PartialEq + Eq> Phase<Bn> {
        /// Weather the phase is signed or not.
        pub fn is_signed(&self) -> bool {
            match self {
                Phase::Signed => true,
                _ => false,
            }
        }
        /// Weather the phase is unsigned or not.
        pub fn is_unsigned(&self) -> bool {
            match self {
                Phase::Unsigned(_) => true,
                _ => false,
            }
        }
        /// Weather the phase is unsigned and open or not, with specific start.
        pub fn is_unsigned_open_at(&self, at: Bn) -> bool {
            match self {
                Phase::Unsigned((true, real)) if *real == at => true,
                _ => false,
            }
        }
        /// Weather the phase is unsigned and open or not.
        pub fn is_unsigned_open(&self) -> bool {
            match self {
                Phase::Unsigned((true, _)) => true,
                _ => false,
            }
        }
        /// Weather the phase is off or not.
        pub fn is_off(&self) -> bool {
            match self {
                Phase::Off => true,
                _ => false,
            }
        }
    }
    /// The type of `Computation` that provided this election data.
    pub enum ElectionCompute {
        /// Election was computed on-chain.
        OnChain,
        /// Election was computed with a signed submission.
        Signed,
        /// Election was computed with an unsigned submission.
        Unsigned,
    }
    impl ::core::marker::StructuralPartialEq for ElectionCompute {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl ::core::cmp::PartialEq for ElectionCompute {
        #[inline]
        fn eq(&self, other: &ElectionCompute) -> bool {
            {
                let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
                let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
                if true && __self_vi == __arg_1_vi {
                    match (&*self, &*other) {
                        _ => true,
                    }
                } else {
                    false
                }
            }
        }
    }
    impl ::core::marker::StructuralEq for ElectionCompute {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl ::core::cmp::Eq for ElectionCompute {
        #[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {}
        }
    }
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl ::core::clone::Clone for ElectionCompute {
        #[inline]
        fn clone(&self) -> ElectionCompute {
            {
                *self
            }
        }
    }
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::marker::Copy for ElectionCompute {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for ElectionCompute {
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				match *self {
					ElectionCompute::OnChain => {
						__codec_dest_edqy.push_byte(0usize as u8);
					}
					ElectionCompute::Signed => {
						__codec_dest_edqy.push_byte(1usize as u8);
					}
					ElectionCompute::Unsigned => {
						__codec_dest_edqy.push_byte(2usize as u8);
					}
					_ => (),
				}
			}
		}
		impl _parity_scale_codec::EncodeLike for ElectionCompute {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for ElectionCompute {
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match __codec_input_edqy.read_byte()? {
					__codec_x_edqy if __codec_x_edqy == 0usize as u8 => {
						Ok(ElectionCompute::OnChain)
					}
					__codec_x_edqy if __codec_x_edqy == 1usize as u8 => Ok(ElectionCompute::Signed),
					__codec_x_edqy if __codec_x_edqy == 2usize as u8 => {
						Ok(ElectionCompute::Unsigned)
					}
					_ => Err("No such variant in enum ElectionCompute".into()),
				}
			}
		}
	};
	impl core::fmt::Debug for ElectionCompute {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::OnChain => fmt.debug_tuple("ElectionCompute::OnChain").finish(),
				Self::Signed => fmt.debug_tuple("ElectionCompute::Signed").finish(),
				Self::Unsigned => fmt.debug_tuple("ElectionCompute::Unsigned").finish(),
				_ => Ok(()),
			}
		}
	}
	impl Default for ElectionCompute {
		fn default() -> Self {
			ElectionCompute::OnChain
		}
	}
	/// A raw, unchecked solution.
	///
	/// This is what will get submitted to the chain.
	///
	/// Such a solution should never become effective in anyway before being checked by the
	/// [`Module::feasibility_check`].
	pub struct RawSolution<C> {
		/// Compact election edges.
		compact: C,
		/// The _claimed_ score of the solution.
		score: ElectionScore,
	}
	impl<C> ::core::marker::StructuralPartialEq for RawSolution<C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::cmp::PartialEq> ::core::cmp::PartialEq for RawSolution<C> {
		#[inline]
		fn eq(&self, other: &RawSolution<C>) -> bool {
			match *other {
                RawSolution {
                    compact: ref __self_1_0,
                    score: ref __self_1_1,
                } => match *self {
                    RawSolution {
                        compact: ref __self_0_0,
                        score: ref __self_0_1,
                    } => (*__self_0_0) == (*__self_1_0) && (*__self_0_1) == (*__self_1_1),
                },
            }
        }
        #[inline]
        fn ne(&self, other: &RawSolution<C>) -> bool {
            match *other {
                RawSolution {
                    compact: ref __self_1_0,
                    score: ref __self_1_1,
                } => match *self {
                    RawSolution {
                        compact: ref __self_0_0,
                        score: ref __self_0_1,
                    } => (*__self_0_0) != (*__self_1_0) || (*__self_0_1) != (*__self_1_1),
                },
            }
		}
	}
	impl<C> ::core::marker::StructuralEq for RawSolution<C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::cmp::Eq> ::core::cmp::Eq for RawSolution<C> {
		#[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {
                let _: ::core::cmp::AssertParamIsEq<C>;
                let _: ::core::cmp::AssertParamIsEq<ElectionScore>;
            }
        }
	}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<C: ::core::clone::Clone> ::core::clone::Clone for RawSolution<C> {
        #[inline]
        fn clone(&self) -> RawSolution<C> {
            match *self {
                RawSolution {
                    compact: ref __self_0_0,
                    score: ref __self_0_1,
                } => RawSolution {
                    compact: ::core::clone::Clone::clone(&(*__self_0_0)),
                    score: ::core::clone::Clone::clone(&(*__self_0_1)),
                },
            }
        }
    }
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<C> _parity_scale_codec::Encode for RawSolution<C>
		where
			C: _parity_scale_codec::Encode,
			C: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				__codec_dest_edqy.push(&self.compact);
				__codec_dest_edqy.push(&self.score);
			}
		}
		impl<C> _parity_scale_codec::EncodeLike for RawSolution<C>
		where
			C: _parity_scale_codec::Encode,
			C: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<C> _parity_scale_codec::Decode for RawSolution<C>
		where
			C: _parity_scale_codec::Decode,
			C: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(RawSolution {
					compact: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field RawSolution.compact".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					score: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field RawSolution.score".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
				})
			}
		}
	};
	impl<C> core::fmt::Debug for RawSolution<C>
	where
		C: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("RawSolution")
				.field("compact", &self.compact)
				.field("score", &self.score)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::default::Default> ::core::default::Default for RawSolution<C> {
		#[inline]
        fn default() -> RawSolution<C> {
            RawSolution {
                compact: ::core::default::Default::default(),
                score: ::core::default::Default::default(),
            }
        }
	}
    /// A raw, unchecked signed submission.
    ///
    /// This is just a wrapper around [`RawSolution`] and some additional info.
    pub struct SignedSubmission<A, B: HasCompact, C> {
        /// Who submitted this solution.
        who: A,
        /// The deposit reserved for storing this solution.
        deposit: B,
        /// The reward that should be given to this solution, if chosen the as the final one.
        reward: B,
        /// The raw solution itself.
        solution: RawSolution<C>,
    }
    impl<A, B: HasCompact, C> ::core::marker::StructuralPartialEq for SignedSubmission<A, B, C> {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<
            A: ::core::cmp::PartialEq,
            B: ::core::cmp::PartialEq + HasCompact,
            C: ::core::cmp::PartialEq,
        > ::core::cmp::PartialEq for SignedSubmission<A, B, C>
    {
        #[inline]
        fn eq(&self, other: &SignedSubmission<A, B, C>) -> bool {
            match *other {
                SignedSubmission {
                    who: ref __self_1_0,
                    deposit: ref __self_1_1,
                    reward: ref __self_1_2,
                    solution: ref __self_1_3,
                } => match *self {
                    SignedSubmission {
                        who: ref __self_0_0,
                        deposit: ref __self_0_1,
                        reward: ref __self_0_2,
                        solution: ref __self_0_3,
                    } => {
                        (*__self_0_0) == (*__self_1_0)
                            && (*__self_0_1) == (*__self_1_1)
                            && (*__self_0_2) == (*__self_1_2)
                            && (*__self_0_3) == (*__self_1_3)
                    }
                },
            }
        }
        #[inline]
        fn ne(&self, other: &SignedSubmission<A, B, C>) -> bool {
            match *other {
                SignedSubmission {
                    who: ref __self_1_0,
                    deposit: ref __self_1_1,
                    reward: ref __self_1_2,
                    solution: ref __self_1_3,
                } => match *self {
                    SignedSubmission {
                        who: ref __self_0_0,
                        deposit: ref __self_0_1,
                        reward: ref __self_0_2,
                        solution: ref __self_0_3,
                    } => {
                        (*__self_0_0) != (*__self_1_0)
                            || (*__self_0_1) != (*__self_1_1)
                            || (*__self_0_2) != (*__self_1_2)
                            || (*__self_0_3) != (*__self_1_3)
                    }
                },
            }
        }
    }
    impl<A, B: HasCompact, C> ::core::marker::StructuralEq for SignedSubmission<A, B, C> {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<A: ::core::cmp::Eq, B: ::core::cmp::Eq + HasCompact, C: ::core::cmp::Eq> ::core::cmp::Eq
        for SignedSubmission<A, B, C>
    {
        #[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {
                let _: ::core::cmp::AssertParamIsEq<A>;
                let _: ::core::cmp::AssertParamIsEq<B>;
                let _: ::core::cmp::AssertParamIsEq<B>;
                let _: ::core::cmp::AssertParamIsEq<RawSolution<C>>;
            }
        }
    }
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<
            A: ::core::clone::Clone,
            B: ::core::clone::Clone + HasCompact,
            C: ::core::clone::Clone,
        > ::core::clone::Clone for SignedSubmission<A, B, C>
    {
        #[inline]
        fn clone(&self) -> SignedSubmission<A, B, C> {
            match *self {
                SignedSubmission {
                    who: ref __self_0_0,
                    deposit: ref __self_0_1,
                    reward: ref __self_0_2,
                    solution: ref __self_0_3,
                } => SignedSubmission {
                    who: ::core::clone::Clone::clone(&(*__self_0_0)),
                    deposit: ::core::clone::Clone::clone(&(*__self_0_1)),
                    reward: ::core::clone::Clone::clone(&(*__self_0_2)),
                    solution: ::core::clone::Clone::clone(&(*__self_0_3)),
                },
            }
        }
    }
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A, B: HasCompact, C> _parity_scale_codec::Encode for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Encode,
			A: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				__codec_dest_edqy.push(&self.who);
				__codec_dest_edqy.push(&self.deposit);
				__codec_dest_edqy.push(&self.reward);
				__codec_dest_edqy.push(&self.solution);
			}
		}
		impl<A, B: HasCompact, C> _parity_scale_codec::EncodeLike for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Encode,
			A: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A, B: HasCompact, C> _parity_scale_codec::Decode for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Decode,
			A: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			RawSolution<C>: _parity_scale_codec::Decode,
			RawSolution<C>: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(SignedSubmission {
					who: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.who".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					deposit: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.deposit".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					reward: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.reward".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					solution: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.solution".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
				})
			}
		}
	};
	impl<A, B: HasCompact, C> core::fmt::Debug for SignedSubmission<A, B, C>
	where
		A: core::fmt::Debug,
		B: core::fmt::Debug,
		C: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("SignedSubmission")
				.field("who", &self.who)
				.field("deposit", &self.deposit)
				.field("reward", &self.reward)
				.field("solution", &self.solution)
				.finish()
		}
	}
	/// A checked solution, ready to be enacted.
	pub struct ReadySolution<A> {
		/// The final supports of the solution.
		///
		/// This is target-major vector, storing each winners, total backing, and each individual
		/// backer.
		supports: Supports<A>,
		/// The score of the solution.
		///
		/// This is needed to potentially challenge the solution.
		score: ElectionScore,
		/// How this election was computed.
		compute: ElectionCompute,
	}
	impl<A> ::core::marker::StructuralPartialEq for ReadySolution<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::PartialEq> ::core::cmp::PartialEq for ReadySolution<A> {
		#[inline]
		fn eq(&self, other: &ReadySolution<A>) -> bool {
			match *other {
				ReadySolution {
					supports: ref __self_1_0,
					score: ref __self_1_1,
					compute: ref __self_1_2,
				} => match *self {
					ReadySolution {
						supports: ref __self_0_0,
						score: ref __self_0_1,
						compute: ref __self_0_2,
					} => {
						(*__self_0_0) == (*__self_1_0)
							&& (*__self_0_1) == (*__self_1_1)
							&& (*__self_0_2) == (*__self_1_2)
					}
				},
			}
		}
        #[inline]
        fn ne(&self, other: &ReadySolution<A>) -> bool {
            match *other {
                ReadySolution {
                    supports: ref __self_1_0,
                    score: ref __self_1_1,
                    compute: ref __self_1_2,
                } => match *self {
                    ReadySolution {
                        supports: ref __self_0_0,
                        score: ref __self_0_1,
                        compute: ref __self_0_2,
                    } => {
                        (*__self_0_0) != (*__self_1_0)
                            || (*__self_0_1) != (*__self_1_1)
                            || (*__self_0_2) != (*__self_1_2)
                    }
                },
            }
		}
	}
	impl<A> ::core::marker::StructuralEq for ReadySolution<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::Eq> ::core::cmp::Eq for ReadySolution<A> {
		#[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {
                let _: ::core::cmp::AssertParamIsEq<Supports<A>>;
                let _: ::core::cmp::AssertParamIsEq<ElectionScore>;
                let _: ::core::cmp::AssertParamIsEq<ElectionCompute>;
            }
        }
	}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<A: ::core::clone::Clone> ::core::clone::Clone for ReadySolution<A> {
        #[inline]
        fn clone(&self) -> ReadySolution<A> {
            match *self {
                ReadySolution {
                    supports: ref __self_0_0,
                    score: ref __self_0_1,
                    compute: ref __self_0_2,
                } => ReadySolution {
                    supports: ::core::clone::Clone::clone(&(*__self_0_0)),
                    score: ::core::clone::Clone::clone(&(*__self_0_1)),
                    compute: ::core::clone::Clone::clone(&(*__self_0_2)),
                },
            }
        }
    }
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Encode for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Encode,
			Supports<A>: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				__codec_dest_edqy.push(&self.supports);
				__codec_dest_edqy.push(&self.score);
				__codec_dest_edqy.push(&self.compute);
			}
		}
		impl<A> _parity_scale_codec::EncodeLike for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Encode,
			Supports<A>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Decode for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Decode,
			Supports<A>: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(ReadySolution {
					supports: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field ReadySolution.supports".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					score: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field ReadySolution.score".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					compute: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field ReadySolution.compute".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
				})
			}
		}
	};
	impl<A> core::fmt::Debug for ReadySolution<A>
	where
		A: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("ReadySolution")
				.field("supports", &self.supports)
				.field("score", &self.score)
				.field("compute", &self.compute)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::default::Default> ::core::default::Default for ReadySolution<A> {
		#[inline]
		fn default() -> ReadySolution<A> {
			ReadySolution {
				supports: ::core::default::Default::default(),
				score: ::core::default::Default::default(),
				compute: ::core::default::Default::default(),
			}
		}
	}
	/// Witness data about the size of the election.
	///
	/// This is needed for proper weight calculation.
	pub struct WitnessData {
		/// Number of all voters.
		///
		/// This must match the on-chain snapshot.
		#[codec(compact)]
		voters: u32,
		/// Number of all targets.
		///
		/// This must match the on-chain snapshot.
		#[codec(compact)]
		targets: u32,
	}
	impl ::core::marker::StructuralPartialEq for WitnessData {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for WitnessData {
		#[inline]
		fn eq(&self, other: &WitnessData) -> bool {
			match *other {
				WitnessData {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
				} => match *self {
					WitnessData {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
					} => (*__self_0_0) == (*__self_1_0) && (*__self_0_1) == (*__self_1_1),
				},
			}
		}
		#[inline]
		fn ne(&self, other: &WitnessData) -> bool {
			match *other {
				WitnessData {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
				} => match *self {
					WitnessData {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
					} => (*__self_0_0) != (*__self_1_0) || (*__self_0_1) != (*__self_1_1),
				},
			}
		}
	}
	impl ::core::marker::StructuralEq for WitnessData {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for WitnessData {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<u32>;
				let _: ::core::cmp::AssertParamIsEq<u32>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::clone::Clone for WitnessData {
		#[inline]
		fn clone(&self) -> WitnessData {
			{
				let _: ::core::clone::AssertParamIsClone<u32>;
				let _: ::core::clone::AssertParamIsClone<u32>;
				*self
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::marker::Copy for WitnessData {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for WitnessData {
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				{
					__codec_dest_edqy . push ( & < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: EncodeAsRef < '_ , u32 > > :: from ( & self . voters ) ) ;
				}
				{
					__codec_dest_edqy . push ( & < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: EncodeAsRef < '_ , u32 > > :: from ( & self . targets ) ) ;
				}
			}
		}
		impl _parity_scale_codec::EncodeLike for WitnessData {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for WitnessData {
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(WitnessData {
					voters: {
						let __codec_res_edqy = < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: Decode > :: decode ( __codec_input_edqy ) ;
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field WitnessData.voters".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy.into(),
						}
					},
					targets: {
						let __codec_res_edqy = < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: Decode > :: decode ( __codec_input_edqy ) ;
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field WitnessData.targets".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy.into(),
						}
					},
				})
			}
		}
	};
	impl core::fmt::Debug for WitnessData {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("WitnessData")
				.field("voters", &self.voters)
				.field("targets", &self.targets)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::default::Default for WitnessData {
		#[inline]
		fn default() -> WitnessData {
			WitnessData {
				voters: ::core::default::Default::default(),
				targets: ::core::default::Default::default(),
			}
		}
	}
	/// A snapshot of all the data that is needed for en entire round. They are provided by
	/// [`ElectionDataProvider`] at the beginning of the signed phase and are kept around until the
	/// round is finished.
	///
	/// These are stored together because they are often times accessed together.
	pub struct RoundSnapshot<A> {
		/// All of the voters.
		pub voters: Vec<(A, VoteWeight, Vec<A>)>,
		/// All of the targets.
		pub targets: Vec<A>,
		/// Desired number of winners to be elected for this round.
		pub desired_targets: u32,
	}
	impl<A> ::core::marker::StructuralPartialEq for RoundSnapshot<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::PartialEq> ::core::cmp::PartialEq for RoundSnapshot<A> {
		#[inline]
		fn eq(&self, other: &RoundSnapshot<A>) -> bool {
			match *other {
				RoundSnapshot {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
					desired_targets: ref __self_1_2,
				} => match *self {
					RoundSnapshot {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
						desired_targets: ref __self_0_2,
					} => {
						(*__self_0_0) == (*__self_1_0)
							&& (*__self_0_1) == (*__self_1_1)
							&& (*__self_0_2) == (*__self_1_2)
					}
				},
			}
		}
		#[inline]
		fn ne(&self, other: &RoundSnapshot<A>) -> bool {
			match *other {
				RoundSnapshot {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
					desired_targets: ref __self_1_2,
				} => match *self {
					RoundSnapshot {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
						desired_targets: ref __self_0_2,
					} => {
						(*__self_0_0) != (*__self_1_0)
							|| (*__self_0_1) != (*__self_1_1)
							|| (*__self_0_2) != (*__self_1_2)
					}
				},
			}
		}
	}
	impl<A> ::core::marker::StructuralEq for RoundSnapshot<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::Eq> ::core::cmp::Eq for RoundSnapshot<A> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<Vec<(A, VoteWeight, Vec<A>)>>;
				let _: ::core::cmp::AssertParamIsEq<Vec<A>>;
				let _: ::core::cmp::AssertParamIsEq<u32>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::clone::Clone> ::core::clone::Clone for RoundSnapshot<A> {
		#[inline]
		fn clone(&self) -> RoundSnapshot<A> {
			match *self {
				RoundSnapshot {
					voters: ref __self_0_0,
					targets: ref __self_0_1,
					desired_targets: ref __self_0_2,
				} => RoundSnapshot {
					voters: ::core::clone::Clone::clone(&(*__self_0_0)),
					targets: ::core::clone::Clone::clone(&(*__self_0_1)),
					desired_targets: ::core::clone::Clone::clone(&(*__self_0_2)),
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Encode for RoundSnapshot<A>
		where
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Encode,
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Encode,
			Vec<A>: _parity_scale_codec::Encode,
			Vec<A>: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				__codec_dest_edqy.push(&self.voters);
				__codec_dest_edqy.push(&self.targets);
				__codec_dest_edqy.push(&self.desired_targets);
			}
		}
		impl<A> _parity_scale_codec::EncodeLike for RoundSnapshot<A>
		where
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Encode,
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Encode,
			Vec<A>: _parity_scale_codec::Encode,
			Vec<A>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Decode for RoundSnapshot<A>
		where
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Decode,
			Vec<(A, VoteWeight, Vec<A>)>: _parity_scale_codec::Decode,
			Vec<A>: _parity_scale_codec::Decode,
			Vec<A>: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(RoundSnapshot {
					voters: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field RoundSnapshot.voters".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					targets: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field RoundSnapshot.targets".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
					desired_targets: {
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err(
									"Error decoding field RoundSnapshot.desired_targets".into()
								)
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					},
				})
			}
		}
	};
	impl<A> core::fmt::Debug for RoundSnapshot<A>
	where
		A: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("RoundSnapshot")
				.field("voters", &self.voters)
				.field("targets", &self.targets)
				.field("desired_targets", &self.desired_targets)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::default::Default> ::core::default::Default for RoundSnapshot<A> {
		#[inline]
		fn default() -> RoundSnapshot<A> {
			RoundSnapshot {
				voters: ::core::default::Default::default(),
				targets: ::core::default::Default::default(),
				desired_targets: ::core::default::Default::default(),
			}
		}
	}
	/// The crate errors.
	///
	/// Note that this is different from the [`PalletError`].
	pub enum Error {
		/// A feasibility error.
		Feasibility(FeasibilityError),
		/// An error in the on-chain fallback.
		OnChainFallback(crate::onchain::Error),
		/// An internal error in the NPoS elections crate.
		NposElections(sp_npos_elections::Error),
		/// Snapshot data was unavailable unexpectedly.
		SnapshotUnAvailable,
		/// Submitting a transaction to the pool failed.
		///
		/// This can only happen in the unsigned phase.
		PoolSubmissionFailed,
	}
	impl core::fmt::Debug for Error {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::Feasibility(ref a0) => {
					fmt.debug_tuple("Error::Feasibility").field(a0).finish()
				}
				Self::OnChainFallback(ref a0) => {
					fmt.debug_tuple("Error::OnChainFallback").field(a0).finish()
				}
				Self::NposElections(ref a0) => {
					fmt.debug_tuple("Error::NposElections").field(a0).finish()
				}
				Self::SnapshotUnAvailable => fmt.debug_tuple("Error::SnapshotUnAvailable").finish(),
				Self::PoolSubmissionFailed => {
					fmt.debug_tuple("Error::PoolSubmissionFailed").finish()
				}
				_ => Ok(()),
			}
		}
	}
	impl ::core::marker::StructuralEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for Error {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<FeasibilityError>;
				let _: ::core::cmp::AssertParamIsEq<crate::onchain::Error>;
				let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
			}
		}
	}
	impl ::core::marker::StructuralPartialEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for Error {
		#[inline]
		fn eq(&self, other: &Error) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Error::Feasibility(ref __self_0), &Error::Feasibility(ref __arg_1_0)) => {
							(*__self_0) == (*__arg_1_0)
						}
						(
							&Error::OnChainFallback(ref __self_0),
							&Error::OnChainFallback(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						(
							&Error::NposElections(ref __self_0),
							&Error::NposElections(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						_ => true,
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &Error) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Error::Feasibility(ref __self_0), &Error::Feasibility(ref __arg_1_0)) => {
							(*__self_0) != (*__arg_1_0)
						}
						(
							&Error::OnChainFallback(ref __self_0),
							&Error::OnChainFallback(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						(
							&Error::NposElections(ref __self_0),
							&Error::NposElections(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						_ => false,
					}
				} else {
					true
				}
			}
		}
	}
	impl From<crate::onchain::Error> for Error {
		fn from(e: crate::onchain::Error) -> Self {
			Error::OnChainFallback(e)
		}
	}
	impl From<sp_npos_elections::Error> for Error {
		fn from(e: sp_npos_elections::Error) -> Self {
			Error::NposElections(e)
		}
	}
	impl From<FeasibilityError> for Error {
		fn from(e: FeasibilityError) -> Self {
			Error::Feasibility(e)
		}
	}
	/// Errors that can happen in the feasibility check.
	pub enum FeasibilityError {
		/// Wrong number of winners presented.
		WrongWinnerCount,
		/// The snapshot is not available.
		///
		/// This must be an internal error of the chain.
		SnapshotUnavailable,
		/// Internal error from the election crate.
		NposElection(sp_npos_elections::Error),
		/// A vote is invalid.
		InvalidVote,
		/// A voter is invalid.
		InvalidVoter,
		/// A winner is invalid.
		InvalidWinner,
		/// The given score was invalid.
		InvalidScore,
	}
	impl core::fmt::Debug for FeasibilityError {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::WrongWinnerCount => fmt
					.debug_tuple("FeasibilityError::WrongWinnerCount")
					.finish(),
				Self::SnapshotUnavailable => fmt
					.debug_tuple("FeasibilityError::SnapshotUnavailable")
					.finish(),
				Self::NposElection(ref a0) => fmt
					.debug_tuple("FeasibilityError::NposElection")
					.field(a0)
					.finish(),
				Self::InvalidVote => fmt.debug_tuple("FeasibilityError::InvalidVote").finish(),
				Self::InvalidVoter => fmt.debug_tuple("FeasibilityError::InvalidVoter").finish(),
				Self::InvalidWinner => fmt.debug_tuple("FeasibilityError::InvalidWinner").finish(),
				Self::InvalidScore => fmt.debug_tuple("FeasibilityError::InvalidScore").finish(),
				_ => Ok(()),
			}
		}
	}
	impl ::core::marker::StructuralEq for FeasibilityError {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for FeasibilityError {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
			}
		}
	}
	impl ::core::marker::StructuralPartialEq for FeasibilityError {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for FeasibilityError {
		#[inline]
		fn eq(&self, other: &FeasibilityError) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&FeasibilityError::NposElection(ref __self_0),
							&FeasibilityError::NposElection(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						_ => true,
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &FeasibilityError) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&FeasibilityError::NposElection(ref __self_0),
							&FeasibilityError::NposElection(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						_ => false,
					}
				} else {
					true
				}
			}
		}
	}
	impl From<sp_npos_elections::Error> for FeasibilityError {
		fn from(e: sp_npos_elections::Error) -> Self {
			FeasibilityError::NposElection(e)
		}
	}
	pub trait WeightInfo {}
	pub trait Config: frame_system::Config + SendTransactionTypes<Call<Self>>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<Self>>>,
	{
		/// Event type.
		type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
		/// Currency type.
		type Currency: ReservableCurrency<Self::AccountId> + Currency<Self::AccountId>;
		/// Duration of the signed phase.
		type SignedPhase: Get<Self::BlockNumber>;
		/// Duration of the unsigned phase.
		type UnsignedPhase: Get<Self::BlockNumber>;
		/// Maximum number of singed submissions that can be queued.
		type MaxSignedSubmissions: Get<u32>;
		type SignedRewardBase: Get<BalanceOf<Self>>;
		type SignedRewardFactor: Get<Perbill>;
		type SignedRewardMax: Get<Option<BalanceOf<Self>>>;
		type SignedDepositBase: Get<BalanceOf<Self>>;
		type SignedDepositByte: Get<BalanceOf<Self>>;
		type SignedDepositWeight: Get<BalanceOf<Self>>;
		/// The minimum amount of improvement to the solution score that defines a solution as "better".
		type SolutionImprovementThreshold: Get<Perbill>;
		/// Maximum number of iteration of balancing that will be executed in the embedded miner of the
		/// pallet.
		type UnsignedMaxIterations: Get<u32>;
		/// The priority of the unsigned transaction submitted in the unsigned-phase
		type UnsignedPriority: Get<TransactionPriority>;
		/// Handler for the slashed deposits.
		type SlashHandler: OnUnbalanced<NegativeImbalanceOf<Self>>;
		/// Handler for the rewards.
		type RewardHandler: OnUnbalanced<PositiveImbalanceOf<Self>>;
		/// Something that will provide the election data.
		type ElectionDataProvider: ElectionDataProvider<Self::AccountId, Self::BlockNumber>;
		/// The weight of the pallet.
		type WeightInfo;
	}
	use self::sp_api_hidden_includes_decl_storage::hidden_include::{
		IterableStorageDoubleMap as _, IterableStorageMap as _, StorageDoubleMap as _,
		StorageMap as _, StoragePrefixedMap as _, StorageValue as _,
	};
	#[doc(hidden)]
	mod sp_api_hidden_includes_decl_storage {
		pub extern crate frame_support as hidden_include;
	}
	trait Store {
		type Round;
		type CurrentPhase;
		type SignedSubmissions;
		type QueuedSolution;
		type Snapshot;
	}
	impl<T: Config + 'static> Store for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Round = Round;
		type CurrentPhase = CurrentPhase<T>;
		type SignedSubmissions = SignedSubmissions<T>;
		type QueuedSolution = QueuedSolution<T>;
		type Snapshot = Snapshot<T>;
	}
	impl<T: Config + 'static> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Internal counter for the number of rounds.
		///
		/// This is useful for de-duplication of transactions submitted to the pool, and general
		/// diagnostics of the module.
		///
		/// This is merely incremented once per every time that signed phase starts.
		pub fn round() -> u32 {
			< Round < > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < u32 > > :: get ( )
		}
		/// Current phase.
		pub fn current_phase() -> Phase<T::BlockNumber> {
			< CurrentPhase < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Phase < T :: BlockNumber > > > :: get ( )
		}
		/// Sorted (worse -> best) list of unchecked, signed solutions.
		pub fn signed_submissions(
		) -> Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>> {
			< SignedSubmissions < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > > > :: get ( )
		}
		/// Current best solution, signed or unsigned.
		pub fn queued_solution() -> Option<ReadySolution<T::AccountId>> {
			< QueuedSolution < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < ReadySolution < T :: AccountId > > > :: get ( )
		}
		/// Snapshot data of the round.
		///
		/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
		pub fn snapshot() -> Option<RoundSnapshot<T::AccountId>> {
			< Snapshot < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < RoundSnapshot < T :: AccountId > > > :: get ( )
		}
	}
	#[doc(hidden)]
	pub struct __GetByteStructRound<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_Round:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Config> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructRound<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_Round
				.get_or_init(|| {
					let def_val: u32 = 0;
					<u32 as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Config> Send for __GetByteStructRound<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Config> Sync for __GetByteStructRound<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructCurrentPhase<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_CurrentPhase:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Config> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructCurrentPhase<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_CurrentPhase
				.get_or_init(|| {
					let def_val: Phase<T::BlockNumber> = Phase::Off;
					<Phase<T::BlockNumber> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Config> Send for __GetByteStructCurrentPhase<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Config> Sync for __GetByteStructCurrentPhase<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructSignedSubmissions<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_SignedSubmissions:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Config> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructSignedSubmissions<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_SignedSubmissions . get_or_init ( | | { let def_val : Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > = Default :: default ( ) ; < Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > as Encode > :: encode ( & def_val ) } ) . clone ( )
		}
	}
	unsafe impl<T: Config> Send for __GetByteStructSignedSubmissions<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Config> Sync for __GetByteStructSignedSubmissions<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructQueuedSolution<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_QueuedSolution:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Config> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructQueuedSolution<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_QueuedSolution
				.get_or_init(|| {
					let def_val: Option<ReadySolution<T::AccountId>> = Default::default();
					<Option<ReadySolution<T::AccountId>> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Config> Send for __GetByteStructQueuedSolution<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Config> Sync for __GetByteStructQueuedSolution<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructSnapshot<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_Snapshot:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Config> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructSnapshot<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_Snapshot
				.get_or_init(|| {
					let def_val: Option<RoundSnapshot<T::AccountId>> = Default::default();
					<Option<RoundSnapshot<T::AccountId>> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Config> Send for __GetByteStructSnapshot<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Config> Sync for __GetByteStructSnapshot<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Config + 'static> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		pub fn storage_metadata(
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::StorageMetadata {
			self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageMetadata { prefix : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "TwoPhaseElectionProvider" ) , entries : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Round" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "u32" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructRound :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Internal counter for the number of rounds." , "" , " This is useful for de-duplication of transactions submitted to the pool, and general" , " diagnostics of the module." , "" , " This is merely incremented once per every time that signed phase starts." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "CurrentPhase" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Phase<T::BlockNumber>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructCurrentPhase :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Current phase." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "SignedSubmissions" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructSignedSubmissions :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Sorted (worse -> best) list of unchecked, signed solutions." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "QueuedSolution" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Optional , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "ReadySolution<T::AccountId>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructQueuedSolution :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Current best solution, signed or unsigned." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Snapshot" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Optional , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "RoundSnapshot<T::AccountId>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructSnapshot :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Snapshot data of the round." , "" , " This is created at the beginning of the signed phase and cleared upon calling `elect`." ] ) , } ] [ .. ] ) , }
		}
	}
	/// Hidden instance generated to be internally used when module is used without
	/// instance.
	#[doc(hidden)]
	pub struct __InherentHiddenInstance;
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::clone::Clone for __InherentHiddenInstance {
		#[inline]
		fn clone(&self) -> __InherentHiddenInstance {
			match *self {
				__InherentHiddenInstance => __InherentHiddenInstance,
			}
		}
	}
	impl ::core::marker::StructuralEq for __InherentHiddenInstance {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for __InherentHiddenInstance {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{}
		}
	}
	impl ::core::marker::StructuralPartialEq for __InherentHiddenInstance {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for __InherentHiddenInstance {
		#[inline]
		fn eq(&self, other: &__InherentHiddenInstance) -> bool {
			match *other {
				__InherentHiddenInstance => match *self {
					__InherentHiddenInstance => true,
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for __InherentHiddenInstance {
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
			}
		}
		impl _parity_scale_codec::EncodeLike for __InherentHiddenInstance {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for __InherentHiddenInstance {
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(__InherentHiddenInstance)
			}
		}
	};
	impl core::fmt::Debug for __InherentHiddenInstance {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_tuple("__InherentHiddenInstance").finish()
		}
	}
	impl self::sp_api_hidden_includes_decl_storage::hidden_include::traits::Instance
		for __InherentHiddenInstance
	{
		const PREFIX: &'static str = "TwoPhaseElectionProvider";
	}
	/// Internal counter for the number of rounds.
	///
	/// This is useful for de-duplication of transactions submitted to the pool, and general
	/// diagnostics of the module.
	///
	/// This is merely incremented once per every time that signed phase starts.
	pub struct Round(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<()>,
	);
	impl
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			u32,
		> for Round
	{
		type Query = u32;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"Round"
		}
		fn from_optional_value_to_query(v: Option<u32>) -> Self::Query {
			v.unwrap_or_else(|| 0)
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<u32> {
			Some(v)
		}
	}
	/// Current phase.
	pub struct CurrentPhase<T: Config>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Config>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Phase<T::BlockNumber>,
		> for CurrentPhase<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Phase<T::BlockNumber>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"CurrentPhase"
		}
		fn from_optional_value_to_query(v: Option<Phase<T::BlockNumber>>) -> Self::Query {
			v.unwrap_or_else(|| Phase::Off)
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<Phase<T::BlockNumber>> {
			Some(v)
		}
	}
	/// Sorted (worse -> best) list of unchecked, signed solutions.
	pub struct SignedSubmissions<T: Config>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Config>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>,
		> for SignedSubmissions<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"SignedSubmissions"
		}
		fn from_optional_value_to_query(
			v: Option<Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>>,
		) -> Self::Query {
			v.unwrap_or_else(|| Default::default())
		}
		fn from_query_to_optional_value(
			v: Self::Query,
		) -> Option<Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>> {
			Some(v)
		}
	}
	/// Current best solution, signed or unsigned.
	pub struct QueuedSolution<T: Config>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Config>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			ReadySolution<T::AccountId>,
		> for QueuedSolution<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Option<ReadySolution<T::AccountId>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"QueuedSolution"
		}
		fn from_optional_value_to_query(v: Option<ReadySolution<T::AccountId>>) -> Self::Query {
			v.or_else(|| Default::default())
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<ReadySolution<T::AccountId>> {
			v
		}
	}
	/// Snapshot data of the round.
	///
	/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
	pub struct Snapshot<T: Config>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Config>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			RoundSnapshot<T::AccountId>,
		> for Snapshot<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Option<RoundSnapshot<T::AccountId>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"Snapshot"
		}
		fn from_optional_value_to_query(v: Option<RoundSnapshot<T::AccountId>>) -> Self::Query {
			v.or_else(|| Default::default())
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<RoundSnapshot<T::AccountId>> {
			v
		}
	}
	/// [`RawEvent`] specialized for the configuration [`Config`]
	///
	/// [`RawEvent`]: enum.RawEvent.html
	/// [`Config`]: trait.Config.html
	pub type Event<T> = RawEvent<<T as frame_system::Config>::AccountId>;
	/// Events for this module.
	///
	pub enum RawEvent<AccountId> {
		/// A solution was stored with the given compute.
		///
		/// If the solution is signed, this means that it hasn't yet been processed. If the solution
		/// is unsigned, this means that it has also been processed.
		SolutionStored(ElectionCompute),
		/// The election has been finalized, with `Some` of the given computation, or else if the
		/// election failed, `None`.
		ElectionFinalized(Option<ElectionCompute>),
		/// An account has been rewarded for their signed submission being finalized.
		Rewarded(AccountId),
		/// An account has been slashed for submitting an invalid signed submission.
		Slashed(AccountId),
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<AccountId: ::core::clone::Clone> ::core::clone::Clone for RawEvent<AccountId> {
		#[inline]
		fn clone(&self) -> RawEvent<AccountId> {
			match (&*self,) {
                (&RawEvent::SolutionStored(ref __self_0),) => {
                    RawEvent::SolutionStored(::core::clone::Clone::clone(&(*__self_0)))
                }
                (&RawEvent::ElectionFinalized(ref __self_0),) => {
                    RawEvent::ElectionFinalized(::core::clone::Clone::clone(&(*__self_0)))
                }
                (&RawEvent::Rewarded(ref __self_0),) => {
                    RawEvent::Rewarded(::core::clone::Clone::clone(&(*__self_0)))
                }
                (&RawEvent::Slashed(ref __self_0),) => {
                    RawEvent::Slashed(::core::clone::Clone::clone(&(*__self_0)))
                }
            }
		}
	}
	impl<AccountId> ::core::marker::StructuralPartialEq for RawEvent<AccountId> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<AccountId: ::core::cmp::PartialEq> ::core::cmp::PartialEq for RawEvent<AccountId> {
		#[inline]
        fn eq(&self, other: &RawEvent<AccountId>) -> bool {
            {
                let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
                let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
                if true && __self_vi == __arg_1_vi {
                    match (&*self, &*other) {
                        (
                            &RawEvent::SolutionStored(ref __self_0),
                            &RawEvent::SolutionStored(ref __arg_1_0),
                        ) => (*__self_0) == (*__arg_1_0),
                        (
                            &RawEvent::ElectionFinalized(ref __self_0),
                            &RawEvent::ElectionFinalized(ref __arg_1_0),
                        ) => (*__self_0) == (*__arg_1_0),
                        (&RawEvent::Rewarded(ref __self_0), &RawEvent::Rewarded(ref __arg_1_0)) => {
                            (*__self_0) == (*__arg_1_0)
                        }
                        (&RawEvent::Slashed(ref __self_0), &RawEvent::Slashed(ref __arg_1_0)) => {
                            (*__self_0) == (*__arg_1_0)
                        }
                        _ => unsafe { ::core::intrinsics::unreachable() },
                    }
                } else {
                    false
                }
            }
        }
        #[inline]
        fn ne(&self, other: &RawEvent<AccountId>) -> bool {
            {
                let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
                let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
                if true && __self_vi == __arg_1_vi {
                    match (&*self, &*other) {
                        (
                            &RawEvent::SolutionStored(ref __self_0),
                            &RawEvent::SolutionStored(ref __arg_1_0),
                        ) => (*__self_0) != (*__arg_1_0),
                        (
                            &RawEvent::ElectionFinalized(ref __self_0),
                            &RawEvent::ElectionFinalized(ref __arg_1_0),
                        ) => (*__self_0) != (*__arg_1_0),
                        (&RawEvent::Rewarded(ref __self_0), &RawEvent::Rewarded(ref __arg_1_0)) => {
                            (*__self_0) != (*__arg_1_0)
                        }
                        (&RawEvent::Slashed(ref __self_0), &RawEvent::Slashed(ref __arg_1_0)) => {
                            (*__self_0) != (*__arg_1_0)
                        }
                        _ => unsafe { ::core::intrinsics::unreachable() },
                    }
                } else {
                    true
                }
            }
        }
	}
    impl<AccountId> ::core::marker::StructuralEq for RawEvent<AccountId> {}
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl<AccountId: ::core::cmp::Eq> ::core::cmp::Eq for RawEvent<AccountId> {
        #[inline]
        #[doc(hidden)]
        fn assert_receiver_is_total_eq(&self) -> () {
            {
                let _: ::core::cmp::AssertParamIsEq<ElectionCompute>;
                let _: ::core::cmp::AssertParamIsEq<Option<ElectionCompute>>;
                let _: ::core::cmp::AssertParamIsEq<AccountId>;
                let _: ::core::cmp::AssertParamIsEq<AccountId>;
            }
        }
    }
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<AccountId> _parity_scale_codec::Encode for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				match *self {
					RawEvent::SolutionStored(ref aa) => {
						__codec_dest_edqy.push_byte(0usize as u8);
						__codec_dest_edqy.push(aa);
					}
					RawEvent::ElectionFinalized(ref aa) => {
						__codec_dest_edqy.push_byte(1usize as u8);
						__codec_dest_edqy.push(aa);
					}
					RawEvent::Rewarded(ref aa) => {
						__codec_dest_edqy.push_byte(2usize as u8);
						__codec_dest_edqy.push(aa);
					}
					RawEvent::Slashed(ref aa) => {
						__codec_dest_edqy.push_byte(3usize as u8);
						__codec_dest_edqy.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<AccountId> _parity_scale_codec::EncodeLike for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<AccountId> _parity_scale_codec::Decode for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match __codec_input_edqy.read_byte()? {
					__codec_x_edqy if __codec_x_edqy == 0usize as u8 => {
						Ok(RawEvent::SolutionStored({
							let __codec_res_edqy =
								_parity_scale_codec::Decode::decode(__codec_input_edqy);
							match __codec_res_edqy {
								Err(_) => {
									return Err(
										"Error decoding field RawEvent :: SolutionStored.0".into()
									)
								}
								Ok(__codec_res_edqy) => __codec_res_edqy,
							}
						}))
					}
					__codec_x_edqy if __codec_x_edqy == 1usize as u8 => {
						Ok(RawEvent::ElectionFinalized({
							let __codec_res_edqy =
								_parity_scale_codec::Decode::decode(__codec_input_edqy);
							match __codec_res_edqy {
								Err(_) => {
									return Err(
										"Error decoding field RawEvent :: ElectionFinalized.0"
											.into(),
									)
								}
								Ok(__codec_res_edqy) => __codec_res_edqy,
							}
						}))
					}
					__codec_x_edqy if __codec_x_edqy == 2usize as u8 => Ok(RawEvent::Rewarded({
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field RawEvent :: Rewarded.0".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					})),
					__codec_x_edqy if __codec_x_edqy == 3usize as u8 => Ok(RawEvent::Slashed({
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => {
								return Err("Error decoding field RawEvent :: Slashed.0".into())
							}
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					})),
					_ => Err("No such variant in enum RawEvent".into()),
				}
			}
		}
	};
	impl<AccountId> core::fmt::Debug for RawEvent<AccountId>
	where
		AccountId: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
                Self::SolutionStored(ref a0) => fmt
                    .debug_tuple("RawEvent::SolutionStored")
                    .field(a0)
                    .finish(),
                Self::ElectionFinalized(ref a0) => fmt
                    .debug_tuple("RawEvent::ElectionFinalized")
                    .field(a0)
                    .finish(),
                Self::Rewarded(ref a0) => fmt.debug_tuple("RawEvent::Rewarded").field(a0).finish(),
                Self::Slashed(ref a0) => fmt.debug_tuple("RawEvent::Slashed").field(a0).finish(),
                _ => Ok(()),
            }
		}
	}
	impl<AccountId> From<RawEvent<AccountId>> for () {
		fn from(_: RawEvent<AccountId>) -> () {
			()
		}
	}
	impl<AccountId> RawEvent<AccountId> {
		#[allow(dead_code)]
		#[doc(hidden)]
		pub fn metadata() -> &'static [::frame_support::event::EventMetadata] {
			&[
                ::frame_support::event::EventMetadata {
                    name: ::frame_support::event::DecodeDifferent::Encode("SolutionStored"),
                    arguments: ::frame_support::event::DecodeDifferent::Encode(&[
                        "ElectionCompute",
                    ]),
                    documentation: ::frame_support::event::DecodeDifferent::Encode(&[
                        r" A solution was stored with the given compute.",
                        r"",
                        r" If the solution is signed, this means that it hasn't yet been processed. If the solution",
                        r" is unsigned, this means that it has also been processed.",
                    ]),
                },
                ::frame_support::event::EventMetadata {
                    name: ::frame_support::event::DecodeDifferent::Encode("ElectionFinalized"),
                    arguments: ::frame_support::event::DecodeDifferent::Encode(&[
                        "Option<ElectionCompute>",
                    ]),
                    documentation: ::frame_support::event::DecodeDifferent::Encode(&[
                        r" The election has been finalized, with `Some` of the given computation, or else if the",
                        r" election failed, `None`.",
                    ]),
                },
                ::frame_support::event::EventMetadata {
                    name: ::frame_support::event::DecodeDifferent::Encode("Rewarded"),
                    arguments: ::frame_support::event::DecodeDifferent::Encode(&["AccountId"]),
                    documentation: ::frame_support::event::DecodeDifferent::Encode(&[
                        r" An account has been rewarded for their signed submission being finalized.",
                    ]),
                },
                ::frame_support::event::EventMetadata {
                    name: ::frame_support::event::DecodeDifferent::Encode("Slashed"),
                    arguments: ::frame_support::event::DecodeDifferent::Encode(&["AccountId"]),
                    documentation: ::frame_support::event::DecodeDifferent::Encode(&[
                        r" An account has been slashed for submitting an invalid signed submission.",
                    ]),
                },
            ]
		}
	}
	pub enum PalletError<T: Config>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		__Ignore(
			::frame_support::sp_std::marker::PhantomData<(T,)>,
			::frame_support::Never,
		),
		/// Submission was too early.
		EarlySubmission,
		/// Submission was too weak, score-wise.
		WeakSubmission,
		/// The queue was full, and the solution was not better than any of the existing ones.
		QueueFull,
		/// The origin failed to pay the deposit.
		CannotPayDeposit,
	}
	impl<T: Config> ::frame_support::sp_std::fmt::Debug for PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn fmt(
			&self,
			f: &mut ::frame_support::sp_std::fmt::Formatter<'_>,
		) -> ::frame_support::sp_std::fmt::Result {
			f.write_str(self.as_str())
		}
	}
	impl<T: Config> PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn as_u8(&self) -> u8 {
			match self {
                PalletError::__Ignore(_, _) => {
                    ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                        &["internal error: entered unreachable code: "],
                        &match (&"`__Ignore` can never be constructed",) {
                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                arg0,
                                ::core::fmt::Display::fmt,
                            )],
                        },
                    ))
                }
                PalletError::EarlySubmission => 0,
                PalletError::WeakSubmission => 0 + 1,
                PalletError::QueueFull => 0 + 1 + 1,
                PalletError::CannotPayDeposit => 0 + 1 + 1 + 1,
            }
		}
		fn as_str(&self) -> &'static str {
			match self {
                Self::__Ignore(_, _) => {
                    ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                        &["internal error: entered unreachable code: "],
                        &match (&"`__Ignore` can never be constructed",) {
                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                arg0,
                                ::core::fmt::Display::fmt,
                            )],
                        },
                    ))
                }
                PalletError::EarlySubmission => "EarlySubmission",
                PalletError::WeakSubmission => "WeakSubmission",
                PalletError::QueueFull => "QueueFull",
                PalletError::CannotPayDeposit => "CannotPayDeposit",
            }
		}
	}
	impl<T: Config> From<PalletError<T>> for &'static str
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn from(err: PalletError<T>) -> &'static str {
			err.as_str()
		}
	}
	impl<T: Config> From<PalletError<T>> for ::frame_support::sp_runtime::DispatchError
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn from(err: PalletError<T>) -> Self {
			let index = <T::PalletInfo as ::frame_support::traits::PalletInfo>::index::<Module<T>>()
				.expect("Every active module has an index in the runtime; qed") as u8;
			::frame_support::sp_runtime::DispatchError::Module {
				index,
				error: err.as_u8(),
				message: Some(err.as_str()),
			}
		}
	}
	impl<T: Config> ::frame_support::error::ModuleErrorMetadata for PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn metadata() -> &'static [::frame_support::error::ErrorMetadata] {
			&[
                ::frame_support::error::ErrorMetadata {
                    name: ::frame_support::error::DecodeDifferent::Encode("EarlySubmission"),
                    documentation: ::frame_support::error::DecodeDifferent::Encode(&[
                        r" Submission was too early.",
                    ]),
                },
                ::frame_support::error::ErrorMetadata {
                    name: ::frame_support::error::DecodeDifferent::Encode("WeakSubmission"),
                    documentation: ::frame_support::error::DecodeDifferent::Encode(&[
                        r" Submission was too weak, score-wise.",
                    ]),
                },
                ::frame_support::error::ErrorMetadata {
                    name: ::frame_support::error::DecodeDifferent::Encode("QueueFull"),
                    documentation: ::frame_support::error::DecodeDifferent::Encode(&[
                        r" The queue was full, and the solution was not better than any of the existing ones.",
                    ]),
                },
                ::frame_support::error::ErrorMetadata {
                    name: ::frame_support::error::DecodeDifferent::Encode("CannotPayDeposit"),
                    documentation: ::frame_support::error::DecodeDifferent::Encode(&[
                        r" The origin failed to pay the deposit.",
                    ]),
                },
            ]
		}
	}
	pub struct Module<T: Config>(::frame_support::sp_std::marker::PhantomData<(T,)>)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::clone::Clone + Config> ::core::clone::Clone for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		fn clone(&self) -> Module<T> {
			match *self {
				Module(ref __self_0_0) => Module(::core::clone::Clone::clone(&(*__self_0_0))),
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::marker::Copy + Config> ::core::marker::Copy for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Config> ::core::marker::StructuralPartialEq for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::cmp::PartialEq + Config> ::core::cmp::PartialEq for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		fn eq(&self, other: &Module<T>) -> bool {
			match *other {
				Module(ref __self_1_0) => match *self {
					Module(ref __self_0_0) => (*__self_0_0) == (*__self_1_0),
				},
			}
		}
		#[inline]
		fn ne(&self, other: &Module<T>) -> bool {
			match *other {
				Module(ref __self_1_0) => match *self {
					Module(ref __self_0_0) => (*__self_0_0) != (*__self_1_0),
				},
			}
		}
	}
	impl<T: Config> ::core::marker::StructuralEq for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::cmp::Eq + Config> ::core::cmp::Eq for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<
					::frame_support::sp_std::marker::PhantomData<(T,)>,
				>;
			}
		}
	}
	impl<T: Config> core::fmt::Debug for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		T: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_tuple("Module").field(&self.0).finish()
		}
	}
	impl<T: frame_system::Config + Config>
		::frame_support::traits::OnInitialize<<T as frame_system::Config>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn on_initialize(now: T::BlockNumber) -> Weight {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
                            "on_initialize",
                            "frame_election_providers::two_phase",
                            ::tracing::Level::TRACE,
                            Some("frame/election-providers/src/two_phase/mod.rs"),
                            Some(469u32),
                            Some("frame_election_providers::two_phase"),
                            ::tracing_core::field::FieldSet::new(
                                &[],
                                ::tracing_core::callsite::Identifier(&CALLSITE),
                            ),
                            ::tracing::metadata::Kind::SPAN,
                        )
                    };
                    MacroCallsite::new(&META)
                };
                let mut interest = ::tracing::subscriber::Interest::never();
                if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
                    && ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
                    && {
                        interest = CALLSITE.interest();
                        !interest.is_never()
                    }
                    && CALLSITE.is_enabled(interest)
                {
                    let meta = CALLSITE.metadata();
                    ::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
                } else {
                    let span = CALLSITE.disabled_span();
                    {};
                    span
                }
			};
			let __tracing_guard__ = __within_span__.enter();
			{
				let next_election = T::ElectionDataProvider::next_election_prediction(now);
				let next_election = next_election.max(now);
				let signed_deadline = T::SignedPhase::get() + T::UnsignedPhase::get();
				let unsigned_deadline = T::UnsignedPhase::get();
				let remaining = next_election - now;
				match Self::current_phase() {
                    Phase::Off if remaining <= signed_deadline && remaining > unsigned_deadline => {
                        <CurrentPhase<T>>::put(Phase::Signed);
                        Round::mutate(|r| *r += 1);
                        Self::start_signed_phase();
                        {
                            let lvl = ::log::Level::Info;
                            if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
                                ::log::__private_api_log(
                                    ::core::fmt::Arguments::new_v1(
                                        &["\u{1f3e6} Starting signed phase at #", " , round "],
                                        &match (&now, &Self::round()) {
                                            (arg0, arg1) => [
                                                ::core::fmt::ArgumentV1::new(
                                                    arg0,
                                                    ::core::fmt::Display::fmt,
                                                ),
                                                ::core::fmt::ArgumentV1::new(
                                                    arg1,
                                                    ::core::fmt::Display::fmt,
                                                ),
                                            ],
                                        },
                                    ),
                                    lvl,
                                    &(
                                        crate::LOG_TARGET,
                                        "frame_election_providers::two_phase",
                                        "frame/election-providers/src/two_phase/mod.rs",
                                        493u32,
                                    ),
                                );
                            }
                        };
                    }
                    Phase::Signed if remaining <= unsigned_deadline && remaining > 0.into() => {
                        let found_solution = Self::finalize_signed_phase();
                        <CurrentPhase<T>>::put(Phase::Unsigned((!found_solution, now)));
                        {
                            let lvl = ::log::Level::Info;
                            if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
                                ::log::__private_api_log(
                                    ::core::fmt::Arguments::new_v1(
                                        &["\u{1f3e6} Starting unsigned phase at #"],
                                        &match (&now,) {
                                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                                arg0,
                                                ::core::fmt::Display::fmt,
                                            )],
                                        },
                                    ),
                                    lvl,
                                    &(
                                        crate::LOG_TARGET,
                                        "frame_election_providers::two_phase",
                                        "frame/election-providers/src/two_phase/mod.rs",
                                        502u32,
                                    ),
                                );
                            }
                        };
                    }
                    _ => {}
                }
				Default::default()
			}
		}
	}
	impl<T: Config> ::frame_support::traits::OnRuntimeUpgrade for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn on_runtime_upgrade() -> ::frame_support::dispatch::Weight {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
							"on_runtime_upgrade",
							"frame_election_providers::two_phase",
							::tracing::Level::TRACE,
							Some("frame/election-providers/src/two_phase/mod.rs"),
							Some(469u32),
							Some("frame_election_providers::two_phase"),
							::tracing_core::field::FieldSet::new(
								&[],
								::tracing_core::callsite::Identifier(&CALLSITE),
							),
							::tracing::metadata::Kind::SPAN,
						)
					};
					MacroCallsite::new(&META)
				};
				let mut interest = ::tracing::subscriber::Interest::never();
				if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
					&& ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
					&& {
						interest = CALLSITE.interest();
						!interest.is_never()
					} && CALLSITE.is_enabled(interest)
				{
					let meta = CALLSITE.metadata();
					::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
				} else {
					let span = CALLSITE.disabled_span();
					{};
					span
				}
			};
			let __tracing_guard__ = __within_span__.enter();
			frame_support::traits::PalletVersion {
				major: 2u16,
				minor: 0u8,
				patch: 0u8,
			}
			.put_into_storage::<<T as frame_system::Config>::PalletInfo, Self>();
			<<T as frame_system::Config>::DbWeight as ::frame_support::traits::Get<_>>::get()
				.writes(1)
		}
	}
	impl<T: frame_system::Config + Config>
		::frame_support::traits::OnFinalize<<T as frame_system::Config>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
	}
	impl<T: frame_system::Config + Config>
		::frame_support::traits::OffchainWorker<<T as frame_system::Config>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn offchain_worker(n: T::BlockNumber) {
			if Self::set_check_offchain_execution_status(n).is_ok()
				&& Self::current_phase().is_unsigned_open_at(n)
			{
				let _ = Self::mine_and_submit().map_err(|e| {
					let lvl = ::log::Level::Error;
					if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
						::log::__private_api_log(
                            ::core::fmt::Arguments::new_v1(
                                &["\u{1f3e6} error while submitting transaction in OCW: "],
                                &match (&e,) {
                                    (arg0,) => [::core::fmt::ArgumentV1::new(
                                        arg0,
                                        ::core::fmt::Debug::fmt,
                                    )],
                                },
                            ),
                            lvl,
                            &(
                                crate::LOG_TARGET,
                                "frame_election_providers::two_phase",
                                "frame/election-providers/src/two_phase/mod.rs",
                                519u32,
                            ),
                        );
					}
				});
			}
		}
	}
	impl<T: Config> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Deposits an event using `frame_system::Module::deposit_event`.
		fn deposit_event(event: impl Into<<T as Config>::Event>) {
			<frame_system::Module<T>>::deposit_event(event.into())
		}
	}
	#[cfg(feature = "std")]
	impl<T: Config> ::frame_support::traits::IntegrityTest for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	/// Can also be called using [`Call`].
	///
	/// [`Call`]: enum.Call.html
	impl<T: Config> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Submit a solution for the signed phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// The solution is potentially queued, based on the claimed score and processed at the end
		/// of the signed phase.
		///
		/// A deposit is reserved and recorded for the solution. Based on the outcome, the solution
		/// might be rewarded, slashed, or get all or a part of the deposit back.
		///
		/// NOTE: Calling this function will bypass origin filters.
		fn submit(
			origin: T::Origin,
			solution: RawSolution<CompactOf<T>>,
		) -> DispatchResultWithPostInfo {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
                            "submit",
                            "frame_election_providers::two_phase",
                            ::tracing::Level::TRACE,
                            Some("frame/election-providers/src/two_phase/mod.rs"),
                            Some(469u32),
                            Some("frame_election_providers::two_phase"),
                            ::tracing_core::field::FieldSet::new(
                                &[],
                                ::tracing_core::callsite::Identifier(&CALLSITE),
                            ),
                            ::tracing::metadata::Kind::SPAN,
                        )
                    };
                    MacroCallsite::new(&META)
                };
                let mut interest = ::tracing::subscriber::Interest::never();
                if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
                    && ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
                    && {
                        interest = CALLSITE.interest();
                        !interest.is_never()
                    }
                    && CALLSITE.is_enabled(interest)
                {
                    let meta = CALLSITE.metadata();
                    ::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
                } else {
                    let span = CALLSITE.disabled_span();
                    {};
                    span
                }
            };
            let __tracing_guard__ = __within_span__.enter();
            let who = ensure_signed(origin)?;
            {
                if !Self::current_phase().is_signed() {
                    {
                        return Err(PalletError::<T>::EarlySubmission.into());
                    };
                }
            };
            let mut signed_submissions = Self::signed_submissions();
            let maybe_index = Self::insert_submission(&who, &mut signed_submissions, solution);
            {
                if !maybe_index.is_some() {
                    {
                        return Err("QueueFull".into());
                    };
                }
            };
            let index = maybe_index.expect("Option checked to be `Some`; qed.");
            let deposit = signed_submissions[index].deposit;
            T::Currency::reserve(&who, deposit).map_err(|_| PalletError::<T>::CannotPayDeposit)?;
            if true {
                if !(signed_submissions.len() as u32 <= T::MaxSignedSubmissions::get()) {
                    {
                        :: std :: rt :: begin_panic ( "assertion failed: signed_submissions.len() as u32 <= T::MaxSignedSubmissions::get()" )
                    }
                };
            };
			<SignedSubmissions<T>>::put(signed_submissions);
			Self::deposit_event(RawEvent::SolutionStored(ElectionCompute::Signed));
			Ok(None.into())
		}
		#[allow(unreachable_code)]
		/// Submit a solution for the unsigned phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// This submission is checked on the fly, thus it is likely yo be more limited and smaller.
		/// Moreover, this unsigned solution is only validated when submitted to the pool from the
		/// local process. Effectively, this means that only active validators can submit this
		/// transaction when authoring a block.
		///
		/// To prevent any incorrect solution (and thus wasted time/weight), this transaction will
		/// panic if the solution submitted by the validator is invalid, effectively putting their
		/// authoring reward at risk.
		///
		/// No deposit or reward is associated with this.
		///
		/// NOTE: Calling this function will bypass origin filters.
		fn submit_unsigned(
			origin: T::Origin,
			solution: RawSolution<CompactOf<T>>,
		) -> ::frame_support::dispatch::DispatchResult {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
                            "submit_unsigned",
                            "frame_election_providers::two_phase",
                            ::tracing::Level::TRACE,
                            Some("frame/election-providers/src/two_phase/mod.rs"),
                            Some(469u32),
                            Some("frame_election_providers::two_phase"),
                            ::tracing_core::field::FieldSet::new(
                                &[],
                                ::tracing_core::callsite::Identifier(&CALLSITE),
                            ),
                            ::tracing::metadata::Kind::SPAN,
                        )
                    };
                    MacroCallsite::new(&META)
                };
                let mut interest = ::tracing::subscriber::Interest::never();
                if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
                    && ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
                    && {
                        interest = CALLSITE.interest();
                        !interest.is_never()
                    }
                    && CALLSITE.is_enabled(interest)
                {
                    let meta = CALLSITE.metadata();
                    ::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
                } else {
                    let span = CALLSITE.disabled_span();
                    {};
                    span
                }
			};
			let __tracing_guard__ = __within_span__.enter();
			{
				ensure_none(origin)?;
				let _ = Self::unsigned_pre_dispatch_checks(&solution)?;
				let ready = Self::feasibility_check(solution, ElectionCompute::Unsigned).expect(
					"Invalid unsigned submission must produce invalid block and deprive \
						validator from their authoring reward.",
				);
				<QueuedSolution<T>>::put(ready);
				Self::deposit_event(RawEvent::SolutionStored(ElectionCompute::Unsigned));
			}
			Ok(())
		}
	}
	/// Dispatchable calls.
	///
	/// Each variant of this enum maps to a dispatchable function from the associated module.
	pub enum Call<T: Config>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[codec(skip)]
		__PhantomItem(
			::frame_support::sp_std::marker::PhantomData<(T,)>,
			::frame_support::Never,
		),
		#[allow(non_camel_case_types)]
		/// Submit a solution for the signed phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// The solution is potentially queued, based on the claimed score and processed at the end
		/// of the signed phase.
		///
		/// A deposit is reserved and recorded for the solution. Based on the outcome, the solution
		/// might be rewarded, slashed, or get all or a part of the deposit back.
		submit(RawSolution<CompactOf<T>>),
		#[allow(non_camel_case_types)]
		/// Submit a solution for the unsigned phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// This submission is checked on the fly, thus it is likely yo be more limited and smaller.
		/// Moreover, this unsigned solution is only validated when submitted to the pool from the
		/// local process. Effectively, this means that only active validators can submit this
		/// transaction when authoring a block.
		///
		/// To prevent any incorrect solution (and thus wasted time/weight), this transaction will
		/// panic if the solution submitted by the validator is invalid, effectively putting their
		/// authoring reward at risk.
		///
		/// No deposit or reward is associated with this.
		submit_unsigned(RawSolution<CompactOf<T>>),
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<T: Config> _parity_scale_codec::Encode for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
		{
			fn encode_to<__CodecOutputEdqy: _parity_scale_codec::Output>(
				&self,
				__codec_dest_edqy: &mut __CodecOutputEdqy,
			) {
				match *self {
					Call::submit(ref aa) => {
						__codec_dest_edqy.push_byte(0usize as u8);
						__codec_dest_edqy.push(aa);
					}
					Call::submit_unsigned(ref aa) => {
						__codec_dest_edqy.push_byte(1usize as u8);
						__codec_dest_edqy.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<T: Config> _parity_scale_codec::EncodeLike for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<T: Config> _parity_scale_codec::Decode for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
		{
			fn decode<__CodecInputEdqy: _parity_scale_codec::Input>(
				__codec_input_edqy: &mut __CodecInputEdqy,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match __codec_input_edqy.read_byte()? {
					__codec_x_edqy if __codec_x_edqy == 0usize as u8 => Ok(Call::submit({
						let __codec_res_edqy =
							_parity_scale_codec::Decode::decode(__codec_input_edqy);
						match __codec_res_edqy {
							Err(_) => return Err("Error decoding field Call :: submit.0".into()),
							Ok(__codec_res_edqy) => __codec_res_edqy,
						}
					})),
					__codec_x_edqy if __codec_x_edqy == 1usize as u8 => {
						Ok(Call::submit_unsigned({
							let __codec_res_edqy =
								_parity_scale_codec::Decode::decode(__codec_input_edqy);
							match __codec_res_edqy {
								Err(_) => {
									return Err(
										"Error decoding field Call :: submit_unsigned.0".into()
									)
								}
								Ok(__codec_res_edqy) => __codec_res_edqy,
							}
						}))
					}
					_ => Err("No such variant in enum Call".into()),
				}
			}
		}
	};
	impl<T: Config> ::frame_support::dispatch::GetDispatchInfo for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn get_dispatch_info(&self) -> ::frame_support::dispatch::DispatchInfo {
			match *self {
                Call::submit(ref solution) => {
                    let base_weight = T::WeightInfo::submit();
                    let weight = <dyn ::frame_support::dispatch::WeighData<(
                        &RawSolution<CompactOf<T>>,
                    )>>::weigh_data(&base_weight, (solution,));
                    let class =
                        <dyn ::frame_support::dispatch::ClassifyDispatch<(
                            &RawSolution<CompactOf<T>>,
                        )>>::classify_dispatch(&base_weight, (solution,));
                    let pays_fee = <dyn ::frame_support::dispatch::PaysFee<(
                        &RawSolution<CompactOf<T>>,
                    )>>::pays_fee(&base_weight, (solution,));
                    ::frame_support::dispatch::DispatchInfo {
                        weight,
                        class,
                        pays_fee,
                    }
                }
                Call::submit_unsigned(ref solution) => {
                    let base_weight = T::WeightInfo::submit_unsigned();
                    let weight = <dyn ::frame_support::dispatch::WeighData<(
                        &RawSolution<CompactOf<T>>,
                    )>>::weigh_data(&base_weight, (solution,));
                    let class =
                        <dyn ::frame_support::dispatch::ClassifyDispatch<(
                            &RawSolution<CompactOf<T>>,
                        )>>::classify_dispatch(&base_weight, (solution,));
                    let pays_fee = <dyn ::frame_support::dispatch::PaysFee<(
                        &RawSolution<CompactOf<T>>,
                    )>>::pays_fee(&base_weight, (solution,));
                    ::frame_support::dispatch::DispatchInfo {
                        weight,
                        class,
                        pays_fee,
                    }
                }
                Call::__PhantomItem(_, _) => {
                    ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                        &["internal error: entered unreachable code: "],
                        &match (&"__PhantomItem should never be used.",) {
                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                arg0,
                                ::core::fmt::Display::fmt,
                            )],
                        },
                    ))
                }
            }
		}
	}
	impl<T: Config> ::frame_support::dispatch::GetCallName for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn get_call_name(&self) -> &'static str {
			match *self {
                Call::submit(ref solution) => {
                    let _ = (solution);
                    "submit"
                }
                Call::submit_unsigned(ref solution) => {
                    let _ = (solution);
                    "submit_unsigned"
                }
                Call::__PhantomItem(_, _) => {
                    ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                        &["internal error: entered unreachable code: "],
                        &match (&"__PhantomItem should never be used.",) {
                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                arg0,
                                ::core::fmt::Display::fmt,
                            )],
                        },
                    ))
                }
            }
		}
		fn get_call_names() -> &'static [&'static str] {
			&["submit", "submit_unsigned"]
		}
	}
	pub use ::frame_support::traits::GetPalletVersion as _;
	impl<T: Config> ::frame_support::traits::GetPalletVersion for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn current_version() -> ::frame_support::traits::PalletVersion {
			frame_support::traits::PalletVersion {
				major: 2u16,
				minor: 0u8,
				patch: 0u8,
			}
		}
		fn storage_version() -> Option<::frame_support::traits::PalletVersion> {
			let key = ::frame_support::traits::PalletVersion::storage_key::<
				<T as frame_system::Config>::PalletInfo,
				Self,
			>()
			.expect("Every active pallet has a name in the runtime; qed");
			::frame_support::storage::unhashed::get(&key)
		}
	}
	impl<T: Config> ::frame_support::traits::OnGenesis for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn on_genesis() {
			frame_support::traits::PalletVersion {
				major: 2u16,
				minor: 0u8,
				patch: 0u8,
			}
			.put_into_storage::<<T as frame_system::Config>::PalletInfo, Self>();
		}
	}
	impl<T: Config> ::frame_support::dispatch::Clone for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn clone(&self) -> Self {
			match *self {
				Call::submit(ref solution) => Call::submit((*solution).clone()),
				Call::submit_unsigned(ref solution) => Call::submit_unsigned((*solution).clone()),
				_ => ::std::rt::begin_panic("internal error: entered unreachable code"),
			}
		}
	}
	impl<T: Config> ::frame_support::dispatch::PartialEq for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn eq(&self, _other: &Self) -> bool {
			match *self {
                Call::submit(ref solution) => {
                    let self_params = (solution,);
                    if let Call::submit(ref solution) = *_other {
                        self_params == (solution,)
                    } else {
                        match *_other {
                            Call::__PhantomItem(_, _) => {
                                ::std::rt::begin_panic("internal error: entered unreachable code")
                            }
                            _ => false,
                        }
                    }
                }
                Call::submit_unsigned(ref solution) => {
                    let self_params = (solution,);
                    if let Call::submit_unsigned(ref solution) = *_other {
                        self_params == (solution,)
                    } else {
                        match *_other {
                            Call::__PhantomItem(_, _) => {
                                ::std::rt::begin_panic("internal error: entered unreachable code")
                            }
                            _ => false,
                        }
                    }
                }
                _ => ::std::rt::begin_panic("internal error: entered unreachable code"),
            }
		}
	}
	impl<T: Config> ::frame_support::dispatch::Eq for Call<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Config> ::frame_support::dispatch::fmt::Debug for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn fmt(
			&self,
			_f: &mut ::frame_support::dispatch::fmt::Formatter,
		) -> ::frame_support::dispatch::result::Result<(), ::frame_support::dispatch::fmt::Error> {
			match *self {
                Call::submit(ref solution) => _f.write_fmt(::core::fmt::Arguments::new_v1(
                    &["", ""],
                    &match (&"submit", &(solution.clone(),)) {
                        (arg0, arg1) => [
                            ::core::fmt::ArgumentV1::new(arg0, ::core::fmt::Display::fmt),
                            ::core::fmt::ArgumentV1::new(arg1, ::core::fmt::Debug::fmt),
                        ],
                    },
                )),
                Call::submit_unsigned(ref solution) => {
                    _f.write_fmt(::core::fmt::Arguments::new_v1(
                        &["", ""],
                        &match (&"submit_unsigned", &(solution.clone(),)) {
                            (arg0, arg1) => [
                                ::core::fmt::ArgumentV1::new(arg0, ::core::fmt::Display::fmt),
                                ::core::fmt::ArgumentV1::new(arg1, ::core::fmt::Debug::fmt),
                            ],
                        },
                    ))
                }
                _ => ::std::rt::begin_panic("internal error: entered unreachable code"),
            }
		}
	}
	impl<T: Config> ::frame_support::traits::UnfilteredDispatchable for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Origin = T::Origin;
		fn dispatch_bypass_filter(
			self,
			_origin: Self::Origin,
		) -> ::frame_support::dispatch::DispatchResultWithPostInfo {
			match self {
                Call::submit(solution) => <Module<T>>::submit(_origin, solution)
                    .map(Into::into)
                    .map_err(Into::into),
                Call::submit_unsigned(solution) => <Module<T>>::submit_unsigned(_origin, solution)
                    .map(Into::into)
                    .map_err(Into::into),
                Call::__PhantomItem(_, _) => {
                    ::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
                        &["internal error: entered unreachable code: "],
                        &match (&"__PhantomItem should never be used.",) {
                            (arg0,) => [::core::fmt::ArgumentV1::new(
                                arg0,
                                ::core::fmt::Display::fmt,
                            )],
                        },
                    ))
                }
            }
		}
	}
	impl<T: Config> ::frame_support::dispatch::Callable<T> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Call = Call<T>;
	}
	impl<T: Config> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[allow(dead_code)]
		pub fn call_functions() -> &'static [::frame_support::dispatch::FunctionMetadata] {
			&[
                ::frame_support::dispatch::FunctionMetadata {
                    name: ::frame_support::dispatch::DecodeDifferent::Encode("submit"),
                    arguments: ::frame_support::dispatch::DecodeDifferent::Encode(&[
                        ::frame_support::dispatch::FunctionArgumentMetadata {
                            name: ::frame_support::dispatch::DecodeDifferent::Encode("solution"),
                            ty: ::frame_support::dispatch::DecodeDifferent::Encode(
                                "RawSolution<CompactOf<T>>",
                            ),
                        },
                    ]),
                    documentation: ::frame_support::dispatch::DecodeDifferent::Encode(&[
                        r" Submit a solution for the signed phase.",
                        r"",
                        r" The dispatch origin fo this call must be __signed__.",
                        r"",
                        r" The solution is potentially queued, based on the claimed score and processed at the end",
                        r" of the signed phase.",
                        r"",
                        r" A deposit is reserved and recorded for the solution. Based on the outcome, the solution",
                        r" might be rewarded, slashed, or get all or a part of the deposit back.",
                    ]),
                },
                ::frame_support::dispatch::FunctionMetadata {
                    name: ::frame_support::dispatch::DecodeDifferent::Encode("submit_unsigned"),
                    arguments: ::frame_support::dispatch::DecodeDifferent::Encode(&[
                        ::frame_support::dispatch::FunctionArgumentMetadata {
                            name: ::frame_support::dispatch::DecodeDifferent::Encode("solution"),
                            ty: ::frame_support::dispatch::DecodeDifferent::Encode(
                                "RawSolution<CompactOf<T>>",
                            ),
                        },
                    ]),
                    documentation: ::frame_support::dispatch::DecodeDifferent::Encode(&[
                        r" Submit a solution for the unsigned phase.",
                        r"",
                        r" The dispatch origin fo this call must be __signed__.",
                        r"",
                        r" This submission is checked on the fly, thus it is likely yo be more limited and smaller.",
                        r" Moreover, this unsigned solution is only validated when submitted to the pool from the",
                        r" local process. Effectively, this means that only active validators can submit this",
                        r" transaction when authoring a block.",
                        r"",
                        r" To prevent any incorrect solution (and thus wasted time/weight), this transaction will",
                        r" panic if the solution submitted by the validator is invalid, effectively putting their",
                        r" authoring reward at risk.",
                        r"",
                        r" No deposit or reward is associated with this.",
                    ]),
                },
            ]
		}
	}
	impl<T: 'static + Config> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[allow(dead_code)]
		pub fn module_constants_metadata(
		) -> &'static [::frame_support::dispatch::ModuleConstantMetadata] {
			&[]
		}
	}
	impl<T: Config> ::frame_support::dispatch::ModuleErrorMetadata for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn metadata() -> &'static [::frame_support::dispatch::ErrorMetadata] {
			<PalletError<T> as ::frame_support::dispatch::ModuleErrorMetadata>::metadata()
		}
	}
	impl<T: Config> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Checks the feasibility of a solution.
		///
		/// This checks the solution for the following:
		///
		/// 0. **all** of the used indices must be correct.
		/// 1. present correct number of winners.
		/// 2. any assignment is checked to match with `SnapshotVoters`.
		/// 3. for each assignment, the check of `ElectionDataProvider` is also examined.
		/// 4. the claimed score is valid.
		fn feasibility_check(
			solution: RawSolution<CompactOf<T>>,
			compute: ElectionCompute,
		) -> Result<ReadySolution<T::AccountId>, FeasibilityError> {
			let RawSolution { compact, score } = solution;
			let winners = compact.unique_targets();
			{
				if !(winners.len() as u32 == Self::desired_targets()) {
					{
						return Err(FeasibilityError::WrongWinnerCount.into());
					};
				}
			};
			let snapshot_voters =
				Self::snapshot_voters().ok_or(FeasibilityError::SnapshotUnavailable)?;
			let snapshot_targets =
				Self::snapshot_targets().ok_or(FeasibilityError::SnapshotUnavailable)?;
			let voter_at = |i: crate::two_phase::CompactVoterIndexOf<T>| -> Option<T::AccountId> {
				<crate::two_phase::CompactVoterIndexOf<T> as crate::TryInto<usize>>::try_into(i)
					.ok()
					.and_then(|i| snapshot_voters.get(i).map(|(x, _, _)| x).cloned())
			};
			let target_at = |i: crate::two_phase::CompactTargetIndexOf<T>| -> Option<T::AccountId> {
				<crate::two_phase::CompactTargetIndexOf<T> as crate::TryInto<usize>>::try_into(i)
					.ok()
					.and_then(|i| snapshot_targets.get(i).cloned())
			};
			let winners = winners
				.into_iter()
				.map(|i| target_at(i).ok_or(FeasibilityError::InvalidWinner))
				.collect::<Result<Vec<T::AccountId>, FeasibilityError>>()?;
            let assignments = compact
                .into_assignment(voter_at, target_at)
                .map_err::<FeasibilityError, _>(Into::into)?;
            let _ = assignments
                .iter()
                .map(|Assignment { who, distribution }| {
                    snapshot_voters.iter().find(|(v, _, _)| v == who).map_or(
                        Err(FeasibilityError::InvalidVoter),
                        |(_, _, t)| {
                            if distribution.iter().map(|(x, _)| x).all(|x| t.contains(x))
                                && T::ElectionDataProvider::feasibility_check_assignment::<
                                    CompactAccuracyOf<T>,
                                >(who, distribution)
                            {
                                Ok(())
                            } else {
                                Err(FeasibilityError::InvalidVote)
                            }
                        },
                    )
                })
                .collect::<Result<(), FeasibilityError>>()?;
            let stake_of = |who: &T::AccountId| -> crate::VoteWeight {
                snapshot_voters
                    .iter()
                    .find(|(x, _, _)| x == who)
                    .map(|(_, x, _)| *x)
                    .unwrap_or_default()
			};
			let staked_assignments = assignment_ratio_to_staked_normalized(assignments, stake_of)
				.map_err::<FeasibilityError, _>(Into::into)?;
			let supports = sp_npos_elections::to_supports(&winners, &staked_assignments)
				.map_err::<FeasibilityError, _>(Into::into)?;
			let known_score = supports.evaluate();
			{
				if !(known_score == score) {
					{
						return Err(FeasibilityError::InvalidScore.into());
					};
				}
			};
			Ok(ReadySolution {
				supports,
				compute,
				score,
			})
		}
		/// On-chain fallback of election.
		fn onchain_fallback() -> Result<Supports<T::AccountId>, Error> {
			let RoundSnapshot {
				desired_targets,
				voters,
				targets,
			} = Self::snapshot().ok_or(Error::SnapshotUnAvailable)?;
			<OnChainSequentialPhragmen as ElectionProvider<T::AccountId>>::elect::<Perbill>(
				desired_targets,
				targets,
				voters,
			)
			.map_err(Into::into)
		}
	}
	impl<T: Config> ElectionProvider<T::AccountId> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		const NEEDS_ELECT_DATA: bool = false;
		type Error = Error;
		fn elect<P: PerThing128>(
			_to_elect: usize,
			_targets: Vec<T::AccountId>,
			_voters: Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>,
		) -> Result<Supports<T::AccountId>, Self::Error>
		where
			ExtendedBalance: From<<P as PerThing>::Inner>,
		{
			Self::queued_solution()
                .map_or_else(
                    || {
                        Self::onchain_fallback()
                            .map(|r| (r, ElectionCompute::OnChain))
                            .map_err(Into::into)
                    },
                    |ReadySolution {
                         supports, compute, ..
                     }| Ok((supports, compute)),
                )
                .map(|(supports, compute)| {
                    <CurrentPhase<T>>::put(Phase::Off);
                    <Snapshot<T>>::kill();
                    Self::deposit_event(RawEvent::ElectionFinalized(Some(compute)));
                    {
                        let lvl = ::log::Level::Info;
                        if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
                            ::log::__private_api_log(
                                ::core::fmt::Arguments::new_v1(
                                    &["\u{1f3e6} Finalized election round with compute ", "."],
                                    &match (&compute,) {
                                        (arg0,) => [::core::fmt::ArgumentV1::new(
                                            arg0,
                                            ::core::fmt::Debug::fmt,
                                        )],
                                    },
                                ),
                                lvl,
                                &(
                                    crate::LOG_TARGET,
                                    "frame_election_providers::two_phase",
                                    "frame/election-providers/src/two_phase/mod.rs",
                                    733u32,
                                ),
                            );
                        }
                    };
                    supports
                })
                .map_err(|err| {
                    Self::deposit_event(RawEvent::ElectionFinalized(None));
                    {
                        let lvl = ::log::Level::Error;
                        if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
                            ::log::__private_api_log(
                                ::core::fmt::Arguments::new_v1(
                                    &["\u{1f3e6} Failed to finalize election round. Error = "],
                                    &match (&err,) {
                                        (arg0,) => [::core::fmt::ArgumentV1::new(
                                            arg0,
                                            ::core::fmt::Debug::fmt,
                                        )],
                                    },
                                ),
                                lvl,
                                &(
                                    crate::LOG_TARGET,
                                    "frame_election_providers::two_phase",
                                    "frame/election-providers/src/two_phase/mod.rs",
                                    738u32,
                                ),
                            );
                        }
                    };
                    err
                })
        }
        fn ongoing() -> bool {
            match Self::current_phase() {
                Phase::Signed | Phase::Unsigned(_) => true,
                _ => false,
            }
		}
	}
}
const LOG_TARGET: &'static str = "election-provider";
#[doc(hidden)]
pub use sp_npos_elections::VoteWeight;
#[doc(hidden)]
pub use sp_runtime::traits::UniqueSaturatedInto;
#[doc(hidden)]
pub use sp_std::convert::TryInto;
