use std::ops::{Deref, DerefMut};

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    program_memory::sol_memcpy,
    pubkey::Pubkey,
};

pub trait Entity
where
    Self: Sized + BorshDeserialize + BorshSerialize,
{
    const SIZE: usize;
    const MAGIC: u8;

    const DATA_SIZE: usize = Self::SIZE - 1;

    fn serialize_to(&self, data: &mut [u8]) -> ProgramResult {
        if data.len() < Self::SIZE {
            return Err(ProgramError::AccountDataTooSmall);
        }

        let serialized = self.try_to_vec()?;
        assert!(
            serialized.len() < Self::SIZE,
            "serialized state to small for this account SIZE"
        );

        data[0] = Self::MAGIC;

        sol_memcpy(&mut data[1..], &serialized, serialized.len());

        Ok(())
    }

    fn deserialize_from(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < Self::SIZE {
            return Err(ProgramError::AccountDataTooSmall);
        }

        if data[0] == 0 {
            return Err(ProgramError::UninitializedAccount);
        }

        if data[0] != Self::MAGIC {
            return Err(ProgramError::InvalidAccountData);
        }

        let state = Self::deserialize(&mut &data[1..])?;

        Ok(state)
    }

    fn is_initialized(data: &[u8]) -> bool {
        data.len() >= Self::SIZE && data[0] != 0
    }
}

pub fn next_entity<'a, 'b: 'a, I: Iterator<Item = &'a AccountInfo<'b>>, T: Entity>(
    i: &mut I,
    program_id: &Pubkey,
) -> Result<(EntityGuard<'a, 'b, T>, &'a AccountInfo<'b>), ProgramError> {
    let state_acc = next_account_info(i)?;

    if state_acc.owner != program_id {
        msg!(
            "{:?} owner: {:?} != {:?}",
            state_acc.key,
            &state_acc.owner,
            program_id
        );
        return Err(ProgramError::IllegalOwner);
    }

    let data = state_acc.try_borrow_data()?;

    let state = Entity::deserialize_from(&data)?;

    Ok((EntityGuard::new(state, state_acc), state_acc))
}

pub fn entity_from_acc<'a, 'b: 'a, T: Entity>(
    acc: &'a AccountInfo<'b>,
    program_id: &Pubkey,
) -> Result<EntityGuard<'a, 'b, T>, ProgramError> {
    if acc.owner != program_id {
        msg!("{:?} owner: {:?} != {:?}", acc.key, &acc.owner, program_id);
        return Err(ProgramError::IllegalOwner);
    }

    let data = acc.try_borrow_data()?;

    let state = Entity::deserialize_from(&data)?;

    Ok(EntityGuard::new(state, acc))
}

// Save entity to account. It is assumed that account address and owner is checked
pub fn initialize_entity<T: Entity>(ent: T, state_acc: &AccountInfo) -> ProgramResult {
    let mut data = state_acc.try_borrow_mut_data()?;

    if T::is_initialized(&data) {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    ent.serialize_to(&mut data)
}

#[derive(Debug)]
pub struct EntityGuard<'a, 'b: 'a, T: Entity> {
    acc: &'a AccountInfo<'b>,
    inner: T,
}

impl<'a, 'b: 'a, T: Entity> EntityGuard<'a, 'b, T> {
    fn new(inner: T, acc: &'a AccountInfo<'b>) -> Self {
        Self { inner, acc }
    }
}

impl<'a, 'b: 'a, T: Entity> Deref for EntityGuard<'a, 'b, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Entity> DerefMut for EntityGuard<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T: Entity> Drop for EntityGuard<'_, '_, T> {
    fn drop(&mut self) {
        let mut data = self
            .acc
            .try_borrow_mut_data()
            .expect("failed to borrow account data");

        self.inner
            .serialize_to(&mut data)
            .expect("error saving entity")
    }
}
