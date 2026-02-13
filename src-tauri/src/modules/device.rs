use rand::{distributions::Alphanumeric, Rng};
use uuid::Uuid;

use crate::models::{DeviceProfile, DeviceProfileVersion};
use crate::modules::account::{load_account, save_account};

/// Generate a new set of device fingerprints (Cursor/VSCode style).
///
/// Produces a DeviceProfile with:
/// - machine_id: `auth0|user_` + 32 random hex chars
/// - mac_machine_id: standard UUID v4 format
/// - dev_device_id: UUID v4
/// - sqm_id: `{UUID-V4-UPPERCASE}`
///
/// Requirement 1.9
pub fn generate_device_profile() -> DeviceProfile {
    DeviceProfile {
        machine_id: format!("auth0|user_{}", random_hex(32)),
        mac_machine_id: new_standard_machine_id(),
        dev_device_id: Uuid::new_v4().to_string(),
        sqm_id: format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase()),
    }
}

/// Bind a device profile to an account, persisting it to disk.
///
/// Requirement 1.9
pub fn bind_device_profile(account_id: &str, profile: DeviceProfile) -> Result<(), String> {
    let mut account = load_account(account_id)?;
    account.device_profile = Some(profile);
    save_account(&account)
}

/// Get device fingerprint history for an account.
pub fn get_device_history(account_id: &str) -> Result<Vec<DeviceProfileVersion>, String> {
    let account = load_account(account_id)?;
    Ok(account.device_history.clone())
}

/// Add a device profile version to the account's history.
///
/// Marks the new entry as `is_current = true` and clears that flag on all
/// previous entries. Also sets the account's active `device_profile` to the
/// new profile.
///
/// Requirement 1.9
pub fn add_device_history(
    account_id: &str,
    label: &str,
    profile: DeviceProfile,
) -> Result<DeviceProfileVersion, String> {
    let mut account = load_account(account_id)?;

    // Clear is_current on existing entries
    for entry in account.device_history.iter_mut() {
        entry.is_current = false;
    }

    let version = DeviceProfileVersion {
        id: Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().timestamp(),
        label: label.to_string(),
        profile: profile.clone(),
        is_current: true,
    };

    account.device_history.push(version.clone());
    account.device_profile = Some(profile);
    save_account(&account)?;

    Ok(version)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn random_hex(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect::<String>()
        .to_lowercase()
}

/// Generate a UUID-v4-style string: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
/// where y ∈ {8, 9, a, b}.
fn new_standard_machine_id() -> String {
    let mut rng = rand::thread_rng();
    let mut id = String::with_capacity(36);
    for ch in "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".chars() {
        match ch {
            '-' | '4' => id.push(ch),
            'y' => id.push_str(&format!("{:x}", rng.gen_range(8..12))),
            _ => id.push_str(&format!("{:x}", rng.gen_range(0..16))),
        }
    }
    id
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_device_profile_format() {
        let profile = generate_device_profile();

        // machine_id starts with "auth0|user_"
        assert!(
            profile.machine_id.starts_with("auth0|user_"),
            "machine_id should start with auth0|user_"
        );
        // The hex part should be 32 chars
        let hex_part = &profile.machine_id["auth0|user_".len()..];
        assert_eq!(hex_part.len(), 32);
        assert!(hex_part.chars().all(|c| c.is_ascii_alphanumeric()));

        // mac_machine_id is UUID-like (36 chars with dashes)
        assert_eq!(profile.mac_machine_id.len(), 36);
        assert!(profile.mac_machine_id.contains('-'));
        // Position 14 should be '4' (version nibble)
        assert_eq!(profile.mac_machine_id.chars().nth(14), Some('4'));

        // dev_device_id is a valid UUID v4
        assert!(Uuid::parse_str(&profile.dev_device_id).is_ok());

        // sqm_id is {UUID-UPPERCASE}
        assert!(profile.sqm_id.starts_with('{'));
        assert!(profile.sqm_id.ends_with('}'));
        let inner = &profile.sqm_id[1..profile.sqm_id.len() - 1];
        assert!(inner.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn test_generate_device_profile_uniqueness() {
        let p1 = generate_device_profile();
        let p2 = generate_device_profile();

        assert_ne!(p1.machine_id, p2.machine_id);
        assert_ne!(p1.dev_device_id, p2.dev_device_id);
        assert_ne!(p1.sqm_id, p2.sqm_id);
    }

    #[test]
    fn test_new_standard_machine_id_format() {
        for _ in 0..20 {
            let id = new_standard_machine_id();
            assert_eq!(id.len(), 36);
            let parts: Vec<&str> = id.split('-').collect();
            assert_eq!(parts.len(), 5);
            assert_eq!(parts[0].len(), 8);
            assert_eq!(parts[1].len(), 4);
            assert_eq!(parts[2].len(), 4);
            assert_eq!(parts[3].len(), 4);
            assert_eq!(parts[4].len(), 12);
            // Version nibble
            assert!(parts[2].starts_with('4'));
            // Variant nibble
            let variant = parts[3].chars().next().unwrap();
            assert!(
                ['8', '9', 'a', 'b'].contains(&variant),
                "variant nibble should be 8-b, got {}",
                variant
            );
        }
    }

    #[test]
    fn test_random_hex_length_and_chars() {
        let hex = random_hex(64);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_alphanumeric()));
        // Should be lowercase
        assert_eq!(hex, hex.to_lowercase());
    }
}
