//! Кодеки G.711: PCMU (μ-law, payload type 0) и PCMA (A-law, payload type 8).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Pcmu,
    Pcma,
}

impl Codec {
    pub fn payload_type(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 8,
        }
    }

    pub fn from_payload_type(pt: u8) -> Option<Codec> {
        match pt {
            0 => Some(Codec::Pcmu),
            8 => Some(Codec::Pcma),
            _ => None,
        }
    }

    pub fn encode(self, samples: &[i16]) -> Vec<u8> {
        match self {
            Codec::Pcmu => samples.iter().map(|&s| linear_to_ulaw(s)).collect(),
            Codec::Pcma => samples.iter().map(|&s| linear_to_alaw(s)).collect(),
        }
    }

    pub fn decode(self, data: &[u8]) -> Vec<i16> {
        match self {
            Codec::Pcmu => data.iter().map(|&b| ulaw_to_linear(b)).collect(),
            Codec::Pcma => data.iter().map(|&b| alaw_to_linear(b)).collect(),
        }
    }
}

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;

pub fn linear_to_ulaw(sample: i16) -> u8 {
    let mut s = sample as i32;
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    if s > ULAW_CLIP {
        s = ULAW_CLIP;
    }
    s += ULAW_BIAS;
    let exponent = 31 - ((s >> 7) as u32).leading_zeros() as i32;
    let mantissa = (s >> (exponent + 3)) & 0x0F;
    !(sign | (exponent << 4) | mantissa) as u8
}

pub fn ulaw_to_linear(byte: u8) -> i16 {
    let u = !byte as i32;
    let exponent = (u >> 4) & 0x07;
    let mantissa = u & 0x0F;
    let magnitude = (((mantissa << 3) + ULAW_BIAS) << exponent) - ULAW_BIAS;
    if u & 0x80 != 0 {
        -magnitude as i16
    } else {
        magnitude as i16
    }
}

pub fn linear_to_alaw(sample: i16) -> u8 {
    const SEG_END: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
    let mut pcm = (sample as i32) >> 3;
    let mask = if pcm >= 0 {
        0xD5
    } else {
        pcm = -pcm - 1;
        0x55
    };
    let segment = SEG_END.iter().position(|&end| pcm <= end).unwrap_or(8);
    if segment >= 8 {
        return (0x7F ^ mask) as u8;
    }
    let mut value = (segment as i32) << 4;
    if segment < 2 {
        value |= (pcm >> 1) & 0x0F;
    } else {
        value |= (pcm >> segment) & 0x0F;
    }
    (value ^ mask) as u8
}

pub fn alaw_to_linear(byte: u8) -> i16 {
    let a = (byte ^ 0x55) as i32;
    let mut t = (a & 0x0F) << 4;
    let segment = (a & 0x70) >> 4;
    match segment {
        0 => t += 8,
        1 => t += 0x108,
        _ => {
            t += 0x108;
            t <<= segment - 1;
        }
    }
    if a & 0x80 != 0 { t as i16 } else { -t as i16 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_encodes_to_known_bytes() {
        assert_eq!(linear_to_ulaw(0), 0xFF);
        assert_eq!(linear_to_alaw(0), 0xD5);
    }

    #[test]
    fn roundtrip_stays_close() {
        for codec in [Codec::Pcmu, Codec::Pcma] {
            for s in (-32000i32..=32000).step_by(97) {
                let s = s as i16;
                let back = codec.decode(&codec.encode(&[s]))[0];
                let tolerance = (s as i32).abs() / 16 + 16;
                assert!(
                    (back as i32 - s as i32).abs() <= tolerance,
                    "{codec:?}: {s} -> {back}"
                );
            }
        }
    }

    #[test]
    fn payload_types() {
        assert_eq!(Codec::from_payload_type(0), Some(Codec::Pcmu));
        assert_eq!(Codec::from_payload_type(8), Some(Codec::Pcma));
        assert_eq!(Codec::from_payload_type(101), None);
    }
}
