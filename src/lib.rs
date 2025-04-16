#[cfg(test)]
mod tests;

use pinocchio::{
    account_info::AccountInfo,
    entrypoint,
    instruction::{Seed, Signer},
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};
use pinocchio_token::instructions::{Burn, InitilizeAccount3, MintTo, Transfer};
use solana_nostd_sha256::hashv;

pinocchio::entrypoint!(process_instruction);

const MIGRATION_FEE_RAYDIUM: u64 = 6; // In SOL
const INITIAL_VTOKEN: u64 = 1_073_000_000;
const INITIAL_VSOL: u64 = 30;
const TOTAL_SUPPLY: u64 = 1_000_000_000;
const ALLOCATION_AT_MIGRATION: u64 = 800_000_000;
pub const RAND: &[u8; 11] = b"random_seed";
pub const ID: [u8; 32] =
    five8_const::decode_32_const("111111111111111111111111111111111111111");

pub struct BondingCurveState(*const u8);

impl BondingCurveState {
    pub const LEN: usize = 1 + 8 + 32 + 8 + 32 + 8 + 8;

    #[inline(always)]
    pub fn from_account_info_unchecked(account_info: &AccountInfo) -> Self {
        unsafe { Self(account_info.borrow_data_unchecked().as_ptr()) }
    }

    pub fn from_account_info(account_info: &AccountInfo) -> Self {
        assert_eq!(account_info.data_len(), Self::LEN);
        assert_eq!(account_info.owner(), &ID);
        Self::from_account_info_unchecked(account_info)
    }

    pub fn is_initialized(&self) -> bool {
        unsafe { core::ptr::read_unaligned(self.0 as *const u8) != 0 }
    }

    pub fn vtoken_reserve_amount(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.0.add(1) as *const u64) }
    }

    pub fn vtoken_mint(&self) -> [u8; 32] {
        let mut mint = [0u8; 32];
        mint.copy_from_slice(unsafe { core::slice::from_raw_parts(self.0.add(9), 32) });
        mint
    }

    pub fn vsol_reserve_amount(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.0.add(41) as *const u64) }
    }

    pub fn vsol_mint(&self) -> [u8; 32] {
        let mut mint = [0u8; 32];
        mint.copy_from_slice(unsafe { core::slice::from_raw_parts(self.0.add(49), 32) });
        mint
    }

    pub fn total_supply(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.0.add(81) as *const u64) }
    }

    pub fn allocation_at_migration(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.0.add(89) as *const u64) }
    }
}

pub enum BondingCurveInstruction {
    Initialize,
    Buy,
    Sell,
    Migrate,
}

impl TryFrom<&u8> for BondingCurveInstruction {
    type Error = ProgramError;

    fn try_from(data: &u8) -> Result<Self, Self::Error> {
        match data {
            0 => Ok(Self::Initialize),
            1 => Ok(Self::Buy),
            2 => Ok(Self::Sell),
            3 => Ok(Self::Migrate),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminator, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    match BondingCurveInstruction::try_from(discriminator)? {
        BondingCurveInstruction::Initialize => initialize(accounts, data),
        BondingCurveInstruction::Buy => buy(accounts, data),
        BondingCurveInstruction::Sell => sell(accounts, data),
        BondingCurveInstruction::Migrate => migrate(accounts),
    }
}

pub fn initialize(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [state_account, vtoken_mint, state_token_account, vsol_mint, admin] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert!(admin.is_signer());

    let (bump, _data) = data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    unsafe {
        let data_ptr = state_account.borrow_mut_data_unchecked().as_mut_ptr();
        *data_ptr = 1;
        *(data_ptr.add(1) as *mut u64) = INITIAL_VTOKEN;
        *(data_ptr.add(9) as *mut [u8; 32]) = *vtoken_mint.key();
        *(data_ptr.add(41) as *mut u64) = INITIAL_VSOL;
        *(data_ptr.add(49) as *mut [u8; 32]) = *vsol_mint.key();
        *(data_ptr.add(81) as *mut u64) = TOTAL_SUPPLY;
        *(data_ptr.add(89) as *mut u64) = ALLOCATION_AT_MIGRATION;
    }

    let binding = bump.to_le_bytes();
    let seeds = [Seed::from(state_account.key().as_ref()), Seed::from(&binding)];
    let signer = [Signer::from(&seeds)];

    InitilizeAccount3 {
        token: state_token_account,
        owner: state_account.key(),
        mint: vtoken_mint,
    }
    .invoke_signed(&signer)?;

    Ok(())
}

pub fn buy(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [state_account, buyer, buyer_ata, buying_mint, state_token_account, state_mint, _token_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert!(buyer.is_signer());

    let (bump, data) = data
        .split_first()
        .ok_or(pinocchio::program_error::ProgramError::InvalidInstructionData)?;

    let pda = hashv(&[
        state_account.key().as_ref(),
        &[*bump],
        ID.as_ref(),
        RAND,
    ]);

    assert!(pda == *state_token_account.key());

    let bonding_curve_state = BondingCurveState::from_account_info(state_account);

    if !bonding_curve_state.is_initialized() {
        return Err(ProgramError::UninitializedAccount);
    }

    assert!(buyer_ata.owner() == _token_program.key());
    assert!(state_token_account.owner() == _token_program.key());
    assert!(buying_mint.key() == &bonding_curve_state.vtoken_mint());
    assert!(state_mint.key() == &bonding_curve_state.vsol_mint());

    let amount = unsafe { *(data.as_ptr() as *const u64) };

    let total_sol_cost = calculate_cost(
        bonding_curve_state.vtoken_reserve_amount(),
        bonding_curve_state.vsol_reserve_amount(),
        amount,
    );

    if state_account.data_len() != 0 {
        unsafe {
            let data_ptr = state_account.borrow_mut_data_unchecked().as_mut_ptr();
            *(data_ptr.add(1) as *mut [u8; 8]) = bonding_curve_state
                .vtoken_reserve_amount()
                .checked_sub(amount)
                .ok_or(ProgramError::InvalidAccountData)?
                .to_le_bytes();
            *(data_ptr.add(41) as *mut [u8; 8]) = bonding_curve_state
                .vsol_reserve_amount()
                .checked_add(total_sol_cost)
                .ok_or(ProgramError::InvalidAccountData)?
                .to_le_bytes();
        }
    } else {
        return Err(ProgramError::UninitializedAccount);
    }

    Transfer {
        from: buyer_ata,
        to: state_token_account,
        authority: buyer,
        amount: total_sol_cost,
    }
    .invoke()?;

    let binding = bump.to_le_bytes();
    let seeds = [Seed::from(state_account.key().as_ref()), Seed::from(&binding)];
    let signer = [Signer::from(&seeds)];

    MintTo {
        mint: buying_mint,
        token: buyer_ata,
        mint_authority: state_account,
        amount,
    }
    .invoke_signed(&signer)?;

    Ok(())
}

pub fn sell(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [state_account, seller, seller_ata, vtoken_mint, state_token_account, vsol_mint, _token_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let bonding_curve_state = BondingCurveState::from_account_info(state_account);

    if !bonding_curve_state.is_initialized() {
        return Err(ProgramError::UninitializedAccount);
    }

    assert!(seller_ata.owner() == _token_program.key());
    assert!(state_token_account.owner() == _token_program.key());
    assert!(vtoken_mint.key() == &bonding_curve_state.vtoken_mint());
    assert!(vsol_mint.key() == &bonding_curve_state.vsol_mint());

    let amount = unsafe { *(instruction_data.as_ptr() as *const u64) };

    let refund = calculate_refund(
        bonding_curve_state.vtoken_reserve_amount(),
        bonding_curve_state.vsol_reserve_amount(),
        amount,
    );

    if state_account.data_len() != 0 {
        unsafe {
            let data_ptr = state_account.borrow_mut_data_unchecked().as_mut_ptr();
            *(data_ptr.add(1) as *mut [u8; 8]) = bonding_curve_state
                .vtoken_reserve_amount()
                .checked_add(amount)
                .ok_or(ProgramError::InvalidAccountData)?
                .to_le_bytes();
            *(data_ptr.add(41) as *mut [u8; 8]) = bonding_curve_state
                .vsol_reserve_amount()
                .checked_sub(refund)
                .ok_or(ProgramError::InvalidAccountData)?
                .to_le_bytes();
        }
    } else {
        return Err(ProgramError::UninitializedAccount);
    }

    Burn {
        token: seller_ata,
        mint: vtoken_mint,
        authority: seller,
        amount,
    }
    .invoke()?;

    Transfer {
        from: state_token_account,
        to: seller_ata,
        authority: state_account,
        amount: refund,
    }
    .invoke()?;

    Ok(())
}

pub fn migrate(accounts: &[AccountInfo]) -> ProgramResult {
    let [state_account, raydium_account, _token_program] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let bonding_curve_state = BondingCurveState::from_account_info(state_account);

    if !bonding_curve_state.is_initialized() {
        return Err(ProgramError::UninitializedAccount);
    }

    if bonding_curve_state.vtoken_reserve_amount() >= bonding_curve_state.allocation_at_migration() {
        let collateral_collected = bonding_curve_state.vsol_reserve_amount() - INITIAL_VSOL;
        let fees = MIGRATION_FEE_RAYDIUM;
        let sol_to_transfer = collateral_collected - fees;
        msg!("Migrating {} SOL to Raydium.", sol_to_transfer);

        Transfer {
            from: state_account,
            to: raydium_account,
            authority: state_account,
            amount: sol_to_transfer,
        }
        .invoke()?;
    }

    Ok(())
}

fn calculate_refund(vtoken_reserve: u64, vsol_reserve: u64, amount: u64) -> u64 {
    let k = vtoken_reserve * vsol_reserve;
    let new_vtoken_reserve = vtoken_reserve.checked_add(amount).unwrap();
    let new_vsol_reserve = k / new_vtoken_reserve;
    vsol_reserve.checked_sub(new_vsol_reserve).unwrap()
}

fn calculate_cost(vtoken_reserve_amount: u64, vsol_reserve: u64, amount: u64) -> u64 {
    let k = vtoken_reserve_amount * vsol_reserve;
    let new_vtoken_reserve = vtoken_reserve_amount.checked_sub(amount).unwrap();
    let new_vsol_reserve = k / new_vtoken_reserve;
    new_vsol_reserve.checked_sub(vsol_reserve).unwrap()
}
