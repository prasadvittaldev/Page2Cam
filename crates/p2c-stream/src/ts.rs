//! Just enough MPEG-TS parsing for the fan-out: packet size, PIDs, the
//! random-access flag and the PAT/PMT tables (to find the video PID).

/// MPEG-TS packet size.
pub const TS: usize = 188;

/// True if the 188-byte packet `pkt` starts a random-access point: sync
/// byte 0x47, an adaptation field of length > 0, and its
/// `random_access_indicator` set.
pub fn is_random_access(pkt: &[u8]) -> bool {
    pkt.len() >= 6 && pkt[0] == 0x47 && pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x40 != 0
}

/// The packet's PID (13 bits).
pub(crate) fn pid(pkt: &[u8]) -> u16 {
    (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2])
}

/// The packet's payload, if it has one.
fn payload(pkt: &[u8]) -> Option<&[u8]> {
    let afc = (pkt[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let start = if afc & 2 != 0 { 5 + pkt[4] as usize } else { 4 };
    pkt.get(start..).filter(|p| !p.is_empty())
}

/// The PSI section that starts in this packet (payload_unit_start set), with
/// its table id checked, cut to its section length minus the CRC.
fn section(pkt: &[u8], table_id: u8) -> Option<&[u8]> {
    if pkt[1] & 0x40 == 0 {
        return None;
    }
    let p = payload(pkt)?;
    let s = p.get(1 + *p.first()? as usize..)?;
    if s.len() < 3 || s[0] != table_id {
        return None;
    }
    let len = ((usize::from(s[1] & 0x0f)) << 8) | usize::from(s[2]);
    // 3 header bytes + section_length, minus the 4-byte CRC.
    let end = (3 + len).saturating_sub(4).min(s.len());
    Some(&s[..end])
}

/// Stream types that carry video (MPEG-1/2, MPEG-4, H.264, HEVC).
const VIDEO_TYPES: [u8; 5] = [0x01, 0x02, 0x10, 0x1b, 0x24];

/// The latest PAT and PMT packets, and the PIDs they name.
#[derive(Default)]
pub(crate) struct Psi {
    pat: Option<[u8; TS]>,
    pmt_pid: Option<u16>,
    pmt: Option<[u8; TS]>,
    video_pid: Option<u16>,
}

impl Psi {
    /// Remember `pkt` if it is the PAT or the PMT.
    pub(crate) fn see(&mut self, pkt: &[u8]) {
        if pkt.len() != TS || pkt[0] != 0x47 {
            return;
        }
        let p = pid(pkt);
        if p == 0 {
            self.pat = pkt.try_into().ok();
            if let Some(s) = section(pkt, 0) {
                // Program loop from byte 8: program_number(2), PID(2).
                let programs = s.get(8..).unwrap_or_default();
                if let Some(e) = programs
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .find(|e| u16::from_be_bytes([e[0], e[1]]) != 0)
                {
                    self.pmt_pid = Some((u16::from(e[2] & 0x1f) << 8) | u16::from(e[3]));
                }
            }
        } else if Some(p) == self.pmt_pid {
            self.pmt = pkt.try_into().ok();
            if let Some(s) = section(pkt, 2) {
                self.video_pid = pmt_video_pid(s).or(self.video_pid);
            }
        }
    }

    /// True if `pkt` is where a new sender may start: a random-access packet
    /// of the video stream (any stream until the PMT names the video).
    pub(crate) fn starts_here(&self, pkt: &[u8]) -> bool {
        is_random_access(pkt) && self.video_pid.is_none_or(|v| pid(pkt) == v)
    }

    /// The cached PAT then PMT packets (whichever are known).
    pub(crate) fn headers(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 * TS);
        for p in [&self.pat, &self.pmt].into_iter().flatten() {
            v.extend_from_slice(p);
        }
        v
    }
}

fn pmt_video_pid(s: &[u8]) -> Option<u16> {
    let pil = (usize::from(*s.get(10)? & 0x0f) << 8) | usize::from(*s.get(11)?);
    let mut i = 12 + pil;
    while i + 5 <= s.len() {
        let (st, es_pid) = (
            s[i],
            (u16::from(s[i + 1] & 0x1f) << 8) | u16::from(s[i + 2]),
        );
        if VIDEO_TYPES.contains(&st) {
            return Some(es_pid);
        }
        i += 5 + ((usize::from(s[i + 3] & 0x0f) << 8) | usize::from(s[i + 4]));
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A packet on `pid` with payload `body` (padded with 0xff).
    pub(crate) fn packet(pid: u16, start: bool, random_access: bool, body: &[u8]) -> [u8; TS] {
        let mut p = [0xffu8; TS];
        p[0] = 0x47;
        p[1] = ((pid >> 8) as u8 & 0x1f) | if start { 0x40 } else { 0 };
        p[2] = pid as u8;
        let mut at = 4;
        if random_access {
            p[3] = 0x30;
            p[4] = 1;
            p[5] = 0x40;
            at = 6;
        } else {
            p[3] = 0x10;
        }
        p[at..at + body.len()].copy_from_slice(body);
        p
    }

    /// PAT naming PMT PID 0x1000 for program 1.
    pub(crate) fn pat() -> [u8; TS] {
        // pointer, table 0, len 13, tsid, ver, sec, last, prog 1 -> 0x1000, CRC
        packet(
            0,
            true,
            false,
            &[
                0, 0x00, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0x00, 1, 2, 3, 4,
            ],
        )
    }

    /// PMT on 0x1000: audio (AAC) on 0x101, video (H.264) on 0x100.
    pub(crate) fn pmt() -> [u8; TS] {
        packet(
            0x1000,
            true,
            false,
            &[
                0, 0x02, 0xb0, 23, 0, 1, 0xc1, 0, 0, 0xe1, 0x00, 0xf0,
                0x00, // header, PCR, pil 0
                0x0f, 0xe1, 0x01, 0xf0, 0x00, // AAC on 0x101
                0x1b, 0xe1, 0x00, 0xf0, 0x00, // H.264 on 0x100
                1, 2, 3, 4,
            ],
        )
    }

    #[test]
    fn random_access_flag_is_read() {
        let mut p = [0u8; 188];
        p[0] = 0x47;
        p[3] = 0x30;
        p[4] = 7;
        p[5] = 0x40;
        assert!(is_random_access(&p));
        p[5] = 0;
        assert!(!is_random_access(&p));
        p[3] = 0x10;
        p[5] = 0x40;
        assert!(!is_random_access(&p), "no adaptation field");
    }

    #[test]
    fn pat_and_pmt_name_the_video_pid() {
        let mut psi = Psi::default();
        assert!(psi.headers().is_empty());
        psi.see(&pat());
        assert_eq!(psi.pmt_pid, Some(0x1000));
        psi.see(&pmt());
        assert_eq!(psi.video_pid, Some(0x100));
        assert_eq!(psi.headers(), [pat(), pmt()].concat());
        // A random-access audio packet is not a start; a video keyframe is.
        assert!(!psi.starts_here(&packet(0x101, true, true, &[])));
        assert!(!psi.starts_here(&packet(0x100, true, false, &[])));
        assert!(psi.starts_here(&packet(0x100, true, true, &[])));
    }
}
