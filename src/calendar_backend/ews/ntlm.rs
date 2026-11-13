//! NTLMv2 as an HTTP client speaks it to on-premises Exchange (PERSO-256,
//! `.agents/research/2026-11-13-ews-spike.md`, "NTLMv2 exchange").
//!
//! Sans-io: [`negotiate_message`] is the type 1 message, [`Challenge::parse`]
//! reads the server's type 2, and [`authenticate_message`] answers it with the
//! type 3, which carries the NTLMv2 response, the session key exchange, the
//! target name (`http/<host>`) and the MIC over the three messages. Nothing
//! here opens a connection; the transport carries the messages in its
//! `Authorization` headers.
//!
//! The subset is the one TUM's Exchange 2019 asks for and the known-good
//! client sends: Unicode strings, extended session security, key exchange, no
//! channel binding, no signing or sealing keys (HTTP never signs a message).
//! Section numbers point to MS-NLMP. MD4 and HMAC-MD5 come from RustCrypto
//! (`md4`, `md-5`, `hmac`); RC4, needed for 16 bytes of session key, is
//! written here.

use anyhow::{anyhow, bail, Result};
use hmac::{Hmac, Mac};
use md4::{Digest, Md4};
use md5::Md5;

/// The message signature every NTLM message opens with.
const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

/// The flags of the type 1, as the known-good client sent them: UNICODE, OEM,
/// REQUEST_TARGET, SIGN, SEAL, NTLM, ALWAYS_SIGN, EXTENDED_SESSIONSECURITY,
/// VERSION, 128, KEY_EXCH and 56.
pub const NEGOTIATE_FLAGS: u32 = 0xE208_8237;

const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_KEY_EXCH: u32 = 0x4000_0000;

/// The version field mp sends in the type 1 and the type 3, the bytes the
/// known-good client sent; the server does not read it, but its last byte
/// is the NTLM revision (15) the MIC needs announced.
pub const VERSION: [u8; 8] = [0x00, 0x0c, 0x04, 0x00, 0x00, 0x00, 0x00, 0x0f];

/// `MsvAvEOL`, which ends an AV pair list.
const AV_EOL: u16 = 0;
/// `MsvAvFlags`.
const AV_FLAGS: u16 = 6;
/// `MsvAvTimestamp`.
const AV_TIMESTAMP: u16 = 7;
/// `MsvAvTargetName`.
const AV_TARGET_NAME: u16 = 9;
/// `MsvAvFlags` bit: the message carries a MIC.
const AV_FLAG_MIC: u32 = 0x0000_0002;

/// Where the MIC sits in a type 3 with a version field.
const MIC_OFFSET: usize = 72;
/// The type 3 header: 8 descriptors and fields up to and including the MIC.
const AUTHENTICATE_HEADER: usize = 88;

/// The type 1 message (2.2.1.1): no domain and no workstation, the payload
/// offsets pointing at the end of the 40-byte header.
pub fn negotiate_message() -> Vec<u8> {
    let mut msg = Vec::with_capacity(40);
    msg.extend_from_slice(SIGNATURE);
    msg.extend_from_slice(&1u32.to_le_bytes());
    msg.extend_from_slice(&NEGOTIATE_FLAGS.to_le_bytes());
    for _ in 0..2 {
        push_descriptor(&mut msg, 0, 40);
    }
    msg.extend_from_slice(&VERSION);
    msg
}

/// What the client reads of the server's type 2 message (2.2.1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// The AV pairs of the target info, in the server's order, without the
    /// closing `MsvAvEOL`.
    pub av_pairs: Vec<(u16, Vec<u8>)>,
}

impl Challenge {
    /// Read a type 2 message, refusing one that is not NTLMv2-capable: no
    /// target info, or no Unicode.
    pub fn parse(msg: &[u8]) -> Result<Self> {
        if msg.len() < 48 || &msg[..8] != SIGNATURE || u32_at(msg, 8)? != 2 {
            bail!("not an NTLM challenge message");
        }
        let flags = u32_at(msg, 20)?;
        if flags & NEGOTIATE_UNICODE == 0 {
            bail!("the NTLM challenge does not offer Unicode");
        }
        if flags & NEGOTIATE_TARGET_INFO == 0 {
            bail!("the NTLM challenge carries no target info, so NTLMv2 cannot answer it");
        }
        let mut server_challenge = [0u8; 8];
        server_challenge.copy_from_slice(&msg[24..32]);
        let info = field(msg, 40)?;
        let mut av_pairs = Vec::new();
        let mut at = 0;
        loop {
            let id = u16_at(info, at)?;
            let len = usize::from(u16_at(info, at + 2)?);
            let value = info
                .get(at + 4..at + 4 + len)
                .ok_or_else(|| anyhow!("an NTLM AV pair runs past the target info"))?;
            if id == AV_EOL {
                break;
            }
            av_pairs.push((id, value.to_vec()));
            at += 4 + len;
        }
        Ok(Challenge {
            flags,
            server_challenge,
            av_pairs,
        })
    }

    /// The server's `MsvAvTimestamp`, which the NTLMv2 response repeats.
    pub fn timestamp(&self) -> Option<[u8; 8]> {
        self.av_pairs
            .iter()
            .find(|(id, _)| *id == AV_TIMESTAMP)
            .and_then(|(_, value)| value.as_slice().try_into().ok())
    }
}

/// Who logs in: the user name and domain as the type 3 sends them (TUM: the
/// bare TUM ID and an empty domain), the password, and the workstation name
/// the message announces.
pub struct Credentials<'a> {
    pub user: &'a str,
    pub domain: &'a str,
    pub password: &'a str,
    pub workstation: &'a str,
}

/// The random values of one type 3, drawn by the caller so a test can fix
/// them.
#[derive(Clone, Copy)]
pub struct Nonces {
    pub client_challenge: [u8; 8],
    pub exported_session_key: [u8; 16],
}

impl Nonces {
    /// Fresh values from the operating system's generator.
    pub fn random() -> Self {
        use rand::RngCore;
        let mut nonces = Nonces {
            client_challenge: [0; 8],
            exported_session_key: [0; 16],
        };
        let mut rng = rand::rngs::OsRng;
        rng.fill_bytes(&mut nonces.client_challenge);
        rng.fill_bytes(&mut nonces.exported_session_key);
        nonces
    }
}

/// `NTOWFv2` (3.3.2): `HMAC_MD5(MD4(UNICODE(password)),
/// UNICODE(UPPERCASE(user) + domain))`, the domain as sent and not
/// uppercased.
pub fn nt_owf_v2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let nt_hash: [u8; 16] = Md4::digest(utf16le(password)).into();
    let identity = utf16le(&format!("{}{domain}", user.to_uppercase()));
    hmac_md5(&nt_hash, &[&identity])
}

/// The NTLMv2 client blob (`temp` of 3.3.2) over `av_pairs`, which already
/// end with `MsvAvEOL`.
fn client_blob(timestamp: [u8; 8], client_challenge: [u8; 8], av_pairs: &[u8]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(28 + av_pairs.len() + 4);
    blob.extend_from_slice(&[0x01, 0x01, 0, 0, 0, 0, 0, 0]);
    blob.extend_from_slice(&timestamp);
    blob.extend_from_slice(&client_challenge);
    blob.extend_from_slice(&[0; 4]);
    blob.extend_from_slice(av_pairs);
    blob.extend_from_slice(&[0; 4]);
    blob
}

/// What 3.3.2 computes from the response key: the NT response
/// (`NTProofStr` then the blob) and the session base key.
struct V2Response {
    nt_response: Vec<u8>,
    session_base_key: [u8; 16],
}

fn v2_response(response_key: &[u8; 16], server_challenge: &[u8; 8], blob: &[u8]) -> V2Response {
    let nt_proof = hmac_md5(response_key, &[server_challenge, blob]);
    let session_base_key = hmac_md5(response_key, &[&nt_proof]);
    let mut nt_response = nt_proof.to_vec();
    nt_response.extend_from_slice(blob);
    V2Response {
        nt_response,
        session_base_key,
    }
}

/// The AV pairs the type 3's blob carries: the server's in their order, then
/// the target name `spn` and `MsvAvFlags` announcing the MIC, then
/// `MsvAvEOL`.
fn client_av_pairs(challenge: &Challenge, spn: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut push = |id: u16, value: &[u8]| {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(value);
    };
    for (id, value) in &challenge.av_pairs {
        if *id != AV_TARGET_NAME && *id != AV_FLAGS {
            push(*id, value);
        }
    }
    push(AV_TARGET_NAME, &utf16le(spn));
    push(AV_FLAGS, &AV_FLAG_MIC.to_le_bytes());
    push(AV_EOL, &[]);
    out
}

/// The type 3 message (2.2.1.3) answering `challenge_msg`, which `negotiate`
/// opened: the NTLMv2 response with the server's timestamp, an LM response
/// of zeros, the session key exchanged under RC4 and the MIC over the three
/// messages, for the service `spn` (`http/mail.tum.de`).
pub fn authenticate_message(
    negotiate: &[u8],
    challenge_msg: &[u8],
    credentials: &Credentials<'_>,
    spn: &str,
    nonces: Nonces,
) -> Result<Vec<u8>> {
    let challenge = Challenge::parse(challenge_msg)?;
    let timestamp = challenge
        .timestamp()
        .ok_or_else(|| anyhow!("the NTLM challenge carries no timestamp"))?;
    if challenge.flags & NEGOTIATE_EXTENDED_SESSIONSECURITY == 0 {
        bail!("the NTLM challenge refuses extended session security");
    }
    let response_key = nt_owf_v2(credentials.password, credentials.user, credentials.domain);
    let blob = client_blob(
        timestamp,
        nonces.client_challenge,
        &client_av_pairs(&challenge, spn),
    );
    let response = v2_response(&response_key, &challenge.server_challenge, &blob);
    // With the server's timestamp in the response, the LM response is zeros
    // (3.1.5.1.2).
    let lm_response = [0u8; 24];
    let (exported_key, encrypted_key) = if challenge.flags & NEGOTIATE_KEY_EXCH != 0 {
        let encrypted = rc4(&response.session_base_key, &nonces.exported_session_key);
        (nonces.exported_session_key, encrypted)
    } else {
        (response.session_base_key, Vec::new())
    };

    let domain = utf16le(credentials.domain);
    let user = utf16le(credentials.user);
    let workstation = utf16le(credentials.workstation);
    let payload: [&[u8]; 6] = [
        &lm_response,
        &response.nt_response,
        &domain,
        &user,
        &workstation,
        &encrypted_key,
    ];
    let mut msg =
        Vec::with_capacity(AUTHENTICATE_HEADER + payload.iter().map(|p| p.len()).sum::<usize>());
    msg.extend_from_slice(SIGNATURE);
    msg.extend_from_slice(&3u32.to_le_bytes());
    let mut offset = AUTHENTICATE_HEADER;
    for part in payload {
        push_descriptor(&mut msg, part.len(), offset);
        offset += part.len();
    }
    msg.extend_from_slice(&challenge.flags.to_le_bytes());
    msg.extend_from_slice(&VERSION);
    msg.extend_from_slice(&[0u8; 16]);
    for part in payload {
        msg.extend_from_slice(part);
    }
    let mic = hmac_md5(&exported_key, &[negotiate, challenge_msg, &msg]);
    msg[MIC_OFFSET..MIC_OFFSET + 16].copy_from_slice(&mic);
    Ok(msg)
}

/// HMAC-MD5 of the concatenation of `parts`.
fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut mac = <Hmac<Md5> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// RC4 of `data` under `key` (the key schedule, then the keystream XORed
/// over the data), for the 16 bytes of the exchanged session key.
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[usize::from(i)]);
            s.swap(usize::from(i), usize::from(j));
            byte ^ s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))]
        })
        .collect()
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn push_descriptor(msg: &mut Vec<u8>, len: usize, offset: usize) {
    let len = len as u16;
    msg.extend_from_slice(&len.to_le_bytes());
    msg.extend_from_slice(&len.to_le_bytes());
    msg.extend_from_slice(&(offset as u32).to_le_bytes());
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| anyhow!("an NTLM message ends early"))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| anyhow!("an NTLM message ends early"))
}

/// The payload a descriptor at `at` points to.
fn field(msg: &[u8], at: usize) -> Result<&[u8]> {
    let len = usize::from(u16_at(msg, at)?);
    let offset = u32_at(msg, at + 4)? as usize;
    msg.get(offset..offset + len)
        .ok_or_else(|| anyhow!("an NTLM field runs past its message"))
}

#[cfg(test)]
mod tests;
