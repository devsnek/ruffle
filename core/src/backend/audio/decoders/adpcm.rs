use super::Decoder;
use bitstream_io::{BigEndian, BitReader};
use std::io::Read;

pub struct AdpcmDecoder<R: Read> {
    inner: BitReader<R, BigEndian>,
    sample_rate: u16,
    is_stereo: bool,
    bits_per_sample: usize,
    sample_num: u16,
    left_sample: i32,
    left_step_index: i16,
    left_step: i32,
    right_sample: i32,
    right_step_index: i16,
    right_step: i32,
    cur_channel: u8,
}

impl<R: Read> AdpcmDecoder<R> {
    const INDEX_TABLE: [&'static [i16]; 4] = [
        &[-1, 2],
        &[-1, -1, 2, 4],
        &[-1, -1, -1, -1, 2, 4, 6, 8],
        &[-1, -1, -1, -1, -1, -1, -1, -1, 1, 2, 4, 6, 8, 10, 13, 16],
    ];

    const STEP_TABLE: [i32; 89] = [
        7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60,
        66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371,
        408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878,
        2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
        8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086,
        29794, 32767,
    ];

    pub fn new(inner: R, is_stereo: bool, sample_rate: u16) -> Result<Self, std::io::Error> {
        let mut reader = BitReader::new(inner);
        let bits_per_sample = reader.read::<u8>(2)? as usize + 2;

        let left_sample = 0;
        let left_step_index = 0;
        let left_step = 0;
        let right_sample = 0;
        let right_step_index = 0;
        let right_step = 0;
        Ok(Self {
            inner: reader,
            sample_rate,
            is_stereo,
            bits_per_sample,
            sample_num: 0,
            left_sample,
            left_step,
            left_step_index,
            right_sample,
            right_step,
            right_step_index,
            cur_channel: 2,
        })
    }

    pub fn next_sample(&mut self) -> Result<(), std::io::Error> {
        self.cur_channel = 0;

        if self.sample_num == 0 {
            // The initial sample values are NOT byte-aligned.
            self.left_sample = self.inner.read_signed(16)?;
            self.left_step_index = self.inner.read::<u16>(6)? as i16;
            self.left_step = Self::STEP_TABLE[self.left_step_index as usize];
            if self.is_stereo {
                self.right_sample = self.inner.read_signed(16)?;
                self.right_step_index = self.inner.read::<u16>(6)? as i16;
                self.right_step = Self::STEP_TABLE[self.right_step_index as usize];
            }
        }

        self.sample_num = (self.sample_num + 1) % 4095;

        let data: i32 = self.inner.read::<u32>(self.bits_per_sample as u32)? as i32;
        self.left_step = Self::STEP_TABLE[self.left_step_index as usize];

        // (data + 0.5) * step / 2^(bits_per_sample - 2)
        // Data is sign-magnitude, NOT two's complement.
        // TODO(Herschel): Other implementations use some bit-tricks for this.
        let sign_mask = 1 << (self.bits_per_sample - 1);
        let magnitude = data & !sign_mask;
        let delta = (2 * magnitude + 1) * self.left_step / sign_mask;

        if (data & sign_mask) != 0 {
            self.left_sample -= delta;
        } else {
            self.left_sample += delta;
        }
        if self.left_sample < -32768 {
            self.left_sample = 32768;
        } else if self.left_sample > 32767 {
            self.left_sample = 32767;
        }

        let i = magnitude as usize;
        self.left_step_index += Self::INDEX_TABLE[self.bits_per_sample - 2][i];
        if self.left_step_index < 0 {
            self.left_step_index = 0;
        } else if self.left_step_index >= Self::STEP_TABLE.len() as i16 {
            self.left_step_index = Self::STEP_TABLE.len() as i16 - 1;
        }

        if self.is_stereo {
            let data = self.inner.read::<u32>(self.bits_per_sample as u32)? as i32;
            self.right_step = Self::STEP_TABLE[self.right_step_index as usize];

            let sign_mask = 1 << (self.bits_per_sample - 1);
            let magnitude = data & !sign_mask;
            let delta = (2 * magnitude + 1) * self.right_step / sign_mask;

            if (data & sign_mask) != 0 {
                self.right_sample -= delta;
            } else {
                self.right_sample += delta;
            }
            if self.right_sample < -32768 {
                self.right_sample = 32768;
            } else if self.right_sample > 32767 {
                self.right_sample = 32767;
            }

            let i = magnitude as usize;
            self.right_step_index += Self::INDEX_TABLE[self.bits_per_sample - 2][i];
            if self.right_step_index < 0 {
                self.right_step_index = 0;
            } else if self.right_step_index >= Self::STEP_TABLE.len() as i16 {
                self.right_step_index = Self::STEP_TABLE.len() as i16 - 1;
            }
        }

        Ok(())
    }
}

impl<R: Read> Iterator for AdpcmDecoder<R> {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        if self.cur_channel >= if self.is_stereo { 2 } else { 1 } {
            self.next_sample().ok()?;
        }

        let sample = if self.cur_channel == 0 {
            self.left_sample
        } else {
            self.right_sample
        };
        self.cur_channel += 1;
        Some(sample as i16)
    }
}

impl<R: std::io::Read> Decoder for AdpcmDecoder<R> {
    #[inline]
    fn num_channels(&self) -> u8 {
        if self.is_stereo {
            2
        } else {
            1
        }
    }

    #[inline]
    fn sample_rate(&self) -> u16 {
        self.sample_rate
    }
}
