//! The NTLM messages against the MS-NLMP section 4.2 vectors, RC4 against
//! RFC 6229, and the encoder against the messages the spike captured
//! (`tests/fixtures/ews/ntlm_*.hex`, the type 2's server names replaced by
//! placeholders of the same length).

use super::*;

const TYPE1: &str = include_str!("../../../../tests/fixtures/ews/ntlm_type1.hex");
const TYPE2: &str = include_str!("../../../../tests/fixtures/ews/ntlm_type2.hex");
const TYPE3_ZEROED: &str = include_str!("../../../../tests/fixtures/ews/ntlm_type3_zeroed.hex");

fn hex(text: &str) -> Vec<u8> {
    let digits: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
        .collect()
}

/// MS-NLMP 4.2.4.3: the CHALLENGE_MESSAGE of the NTLMv2 example.
const SPEC_CHALLENGE: &str = "
    4e 54 4c 4d 53 53 50 00 02 00 00 00 0c 00 0c 00
    38 00 00 00 33 82 8a e2 01 23 45 67 89 ab cd ef
    00 00 00 00 00 00 00 00 24 00 24 00 44 00 00 00
    06 00 70 17 00 00 00 0f 53 00 65 00 72 00 76 00
    65 00 72 00 02 00 0c 00 44 00 6f 00 6d 00 61 00
    69 00 6e 00 01 00 0c 00 53 00 65 00 72 00 76 00
    65 00 72 00 00 00 00 00";

/// MS-NLMP 4.2.1, Common Values.
const SPEC_CLIENT_CHALLENGE: [u8; 8] = [0xaa; 8];
const SPEC_RANDOM_SESSION_KEY: [u8; 16] = [0x55; 16];
const SPEC_TIME: [u8; 8] = [0; 8];

/// MS-NLMP 4.2.4.1.1: `NTOWFv2("Password", "User", "Domain")`.
#[test]
fn ntowfv2_matches_the_spec() {
    assert_eq!(
        nt_owf_v2("Password", "User", "Domain").to_vec(),
        hex("0c 86 8a 40 3b fd 7a 93 a3 00 1e f2 2e f0 2e 3f")
    );
}

/// MS-NLMP 4.2.4.1.2, 4.2.4.1.3 and the AUTHENTICATE_MESSAGE of 4.2.4.3:
/// the blob (`temp`), the NTProofStr at the head of the NT response, the
/// session base key, and the RC4-encrypted session key.
#[test]
fn the_ntlmv2_response_and_session_key_match_the_spec() {
    let challenge = Challenge::parse(&hex(SPEC_CHALLENGE)).unwrap();
    assert_eq!(challenge.flags, 0xe28a_8233);
    assert_eq!(
        challenge.server_challenge,
        [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
    );
    // The spec's temp carries the server's AV pairs alone, then the EOL.
    let mut pairs = Vec::new();
    for (id, value) in &challenge.av_pairs {
        pairs.extend_from_slice(&id.to_le_bytes());
        pairs.extend_from_slice(&(value.len() as u16).to_le_bytes());
        pairs.extend_from_slice(value);
    }
    pairs.extend_from_slice(&[0, 0, 0, 0]);
    let blob = client_blob(SPEC_TIME, SPEC_CLIENT_CHALLENGE, &pairs);
    assert_eq!(
        blob,
        hex("01 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00
             aa aa aa aa aa aa aa aa 00 00 00 00
             02 00 0c 00 44 00 6f 00 6d 00 61 00 69 00 6e 00
             01 00 0c 00 53 00 65 00 72 00 76 00 65 00 72 00
             00 00 00 00 00 00 00 00")
    );
    let key = nt_owf_v2("Password", "User", "Domain");
    let response = v2_response(&key, &challenge.server_challenge, &blob);
    assert_eq!(
        response.nt_response[..16].to_vec(),
        hex("68 cd 0a b8 51 e5 1c 96 aa bc 92 7b eb ef 6a 1c")
    );
    assert_eq!(&response.nt_response[16..], blob.as_slice());
    assert_eq!(
        response.session_base_key.to_vec(),
        hex("8d e4 0c ca db c1 4a 82 f1 5c b0 ad 0d e9 5c a3")
    );
    assert_eq!(
        rc4(&response.session_base_key, &SPEC_RANDOM_SESSION_KEY),
        hex("c5 da d2 54 4f c9 79 90 94 ce 1c e9 0b c9 d0 3e")
    );
}

/// RFC 6229, the 40-bit key `0x0102030405` at offsets 0 and 16 and the
/// 128-bit key `0x0102...10` at offset 0: RC4 of zeros is its keystream.
#[test]
fn rc4_matches_rfc_6229() {
    let key40 = hex("01 02 03 04 05");
    assert_eq!(
        rc4(&key40, &[0; 32]),
        hex("b2 39 63 05 f0 3d c0 27 cc c3 52 4a 0a 11 18 a8
             69 82 94 4f 18 fc 82 d5 89 c4 03 a4 7a 0d 09 19")
    );
    let key128 = hex("01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10");
    assert_eq!(
        rc4(&key128, &[0; 16]),
        hex("9a c7 cc 9a 60 9d 1e f7 b2 93 28 99 cd e4 1b 97")
    );
}

#[test]
fn the_negotiate_message_is_the_known_good_one() {
    assert_eq!(negotiate_message(), hex(TYPE1));
}

#[test]
fn the_captured_challenge_parses_with_its_timestamp() {
    let challenge = Challenge::parse(&hex(TYPE2)).unwrap();
    assert_eq!(challenge.flags, 0xe289_8235);
    let ids: Vec<u16> = challenge.av_pairs.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [2, 1, 4, 3, 5, 7]);
    assert_eq!(
        challenge.timestamp().unwrap().to_vec(),
        hex("27 c1 93 aa 1f 73 dd 01")
    );
    assert!(Challenge::parse(&hex(TYPE1)).is_err());
    assert!(Challenge::parse(b"NTLMSSP\0").is_err());
}

fn fixture_credentials() -> Credentials<'static> {
    Credentials {
        user: "user000",
        domain: "",
        password: "not the password",
        workstation: "WORKSTNWORKSTNWORKS",
    }
}

const NONCES: Nonces = Nonces {
    client_challenge: [0x11; 8],
    exported_session_key: [0x22; 16],
};

/// The type 3 lays out as the known-good client's did: once its LM and NT
/// responses, MIC and encrypted session key are zeroed (the fixture zeroed
/// the same bytes), it is the captured message byte for byte.
#[test]
fn the_authenticate_message_has_the_known_good_layout() {
    let type1 = negotiate_message();
    let mut msg = authenticate_message(
        &type1,
        &hex(TYPE2),
        &fixture_credentials(),
        "http/mail.tum.de",
        NONCES,
    )
    .unwrap();
    for (start, end) in [(72, 88), (88, 112), (112, 368), (420, 436)] {
        msg[start..end].fill(0);
    }
    assert_eq!(msg, hex(TYPE3_ZEROED));
}

/// The fields the layout test zeroes: the blob repeats the server's
/// timestamp and adds the target name and the MIC flag, the NT proof and the
/// session key follow from the password, and the MIC covers the three
/// messages with the exported key.
#[test]
fn the_authenticate_message_carries_a_valid_response_key_and_mic() {
    let type1 = negotiate_message();
    let type2 = hex(TYPE2);
    let creds = fixture_credentials();
    let msg = authenticate_message(&type1, &type2, &creds, "http/mail.tum.de", NONCES).unwrap();
    assert_eq!(&msg[88..112], &[0u8; 24]);
    let nt = &msg[112..368];
    let blob = &nt[16..];
    assert_eq!(&blob[8..16], &hex("27 c1 93 aa 1f 73 dd 01")[..]);
    assert_eq!(&blob[16..24], &NONCES.client_challenge);
    let mut target = vec![9, 0, 32, 0];
    target.extend(utf16le("http/mail.tum.de"));
    target.extend([6, 0, 4, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(blob.ends_with(&target), "{blob:02x?}");

    let challenge = Challenge::parse(&type2).unwrap();
    let key = nt_owf_v2(creds.password, creds.user, creds.domain);
    let expected = v2_response(&key, &challenge.server_challenge, blob);
    assert_eq!(nt, expected.nt_response.as_slice());
    assert_eq!(
        rc4(&expected.session_base_key, &msg[420..436]),
        NONCES.exported_session_key.to_vec()
    );
    let mut zeroed = msg.clone();
    zeroed[72..88].fill(0);
    let mic = hmac_md5(&NONCES.exported_session_key, &[&type1, &type2, &zeroed]);
    assert_eq!(&msg[72..88], &mic);
}

/// A challenge without the timestamp NTLMv2 repeats is refused rather than
/// answered with a weaker response.
#[test]
fn a_challenge_without_a_timestamp_is_refused() {
    let err = authenticate_message(
        &negotiate_message(),
        &hex(SPEC_CHALLENGE),
        &fixture_credentials(),
        "http/mail.tum.de",
        NONCES,
    )
    .unwrap_err();
    assert!(err.to_string().contains("timestamp"), "{err}");
}
