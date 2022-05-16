// Copyright 2022 Ein Terakawa
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use rust_dap::*;
// use rust_dap::{SwdIo, SwdIoConfig, SwdRequest, DapError};
// use rust_dap::{DAP_TRANSFER_OK, DAP_TRANSFER_WAIT, DAP_TRANSFER_FAULT, /* DAP_TRANSFER_ERROR, */ DAP_TRANSFER_MISMATCH};
use hal::gpio::{bank0, PinId};
use hal::pac;
use hal::pio::PIOExt;
use rp2040_hal as hal;
pub mod pio0 {
    use crate::pio::hal::{self, gpio::FunctionPio0};
    pub type Pin<P> = hal::gpio::Pin<P, FunctionPio0>;
}

pub struct SwdIoSet<C, D> {
    // We only need these infomation at initialization in new() .
    // clk_pin_id: u8,
    // dat_pin_id: u8,
    running_sm: hal::pio::StateMachine<hal::pio::PIO0SM0, hal::pio::Running>,
    rx_fifo: hal::pio::Rx<hal::pio::PIO0SM0>,
    tx_fifo: hal::pio::Tx<hal::pio::PIO0SM0>,
    _pins: core::marker::PhantomData<(C, D)>,
}

impl<C, D> SwdIoSet<pio0::Pin<C>, pio0::Pin<D>>
where
    C: PinId + bank0::BankPinId,
    D: PinId + bank0::BankPinId,
{
    #[rustfmt::skip]
    pub fn new(pio0: pac::PIO0, _: pio0::Pin<C>, _: pio0::Pin<D>, resets: &mut pac::RESETS) -> Self {
        let clk_pin_id = C::DYN.num;
        let dat_pin_id = D::DYN.num;
        // Currently HAL does not provide any way to disable schmitt trigger.
        // unsafe { core::ptr::write_volatile((0x4001C004 + clk_pin_id as u32 * 4) as *mut u32, 0x71 as u32) };
        // unsafe { core::ptr::write_volatile((0x4001C004 + dat_pin_id as u32 * 4) as *mut u32, 0x71 as u32); }

        type Assembler = pio::Assembler<{ pio::RP2040_MAX_PROGRAM_SIZE }>;
        let mut a = Assembler::new_with_side_set(pio::SideSet::new(true, 1, false));
        let mut write_loop = a.label();
        let mut write_loop_enter = a.label();
        let mut read_start = a.label();
        let mut read_loop = a.label();
        let mut read_loop_enter = a.label();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        const HI: u8 = 1;
        const LO: u8 = 0;
        // As we are using side set in optional mode, maximum delay is 7.
        const Q: u8 = 4 - 1; // delay

        a.bind(&mut wrap_target);
        // Get number of bits and direction bit from FIFO to OSR.
        a.pull(false, true);
        // Move number of bits to X register.
        a.out(pio::OutDestination::X, 31);
        // Copy direction bit to Y register.
        a.mov(pio::MovDestination::Y, pio::MovOperation::None, pio::MovSource::OSR);
        // Y == 0 means Read , Y != 0 means Write
        a.jmp(pio::JmpCondition::YIsZero, &mut read_start);

        // Write-Bits
        // Get data from FIFO to OSR.
        a.pull(false, true);
        // We want to prepare output value before setting pin direction.
        a.out(pio::OutDestination::PINS, 1);
        // Use ISP as a temporary store.
        a.mov(pio::MovDestination::ISR, pio::MovOperation::None, pio::MovSource::OSR);
        // Y register has value 1. Copy that 1 to OSR.
        a.mov(pio::MovDestination::OSR, pio::MovOperation::None, pio::MovSource::Y);
        // Set IO direction of SWDIO pin to output.
        a.out(pio::OutDestination::PINDIRS, 1);
        // Restore rest of data from ISR to OSR.
        a.mov(pio::MovDestination::OSR, pio::MovOperation::None, pio::MovSource::ISR);
        // Jump if X register is not 0. with post decrement.
        a.jmp(pio::JmpCondition::XDecNonZero, &mut write_loop_enter);

        // We have updated state of SWDIO pin while keeping SWCLK static.
        // Start over.
        a.jmp(pio::JmpCondition::Always, &mut wrap_target);

        a.bind(&mut write_loop);
        // Output 1-bit. and set SWCLK to High.
        a.set(pio::SetDestination::PINS, HI);
        a.out_with_delay(pio::OutDestination::PINS, 1, match Q { 0 => 0, _ => Q - 1 });
        a.bind(&mut write_loop_enter);
        // Keep looping unless X register is 0. and set SWCLK to Low.
        a.jmp_with_delay_and_side_set(pio::JmpCondition::XDecNonZero, &mut write_loop, Q, LO);
        // Set SWCLK to High.
        a.set(pio::SetDestination::PINS, HI);
        // Start over.
        a.jmp(pio::JmpCondition::Always, &mut wrap_target);

        // Read-Bits
        a.bind(&mut read_start);
        // Set IO direction of SWDIO pin to input.
        a.out_with_delay(pio::OutDestination::PINDIRS, 1, match Q { 0 => 0, _ => Q - 1 });
        // Jump if X register is not 0. with post decrement.
        a.jmp(pio::JmpCondition::XDecNonZero, &mut read_loop_enter);
        // Wait before reading SWDIO pin.
        a.nop_with_delay(Q);
        // Read state of SWDIO pin without clocking SWCLK.
        a.r#in(pio::InSource::PINS, 1);
        // Shift in 31 bits of 0 to MSB of ISR.
        a.r#in(pio::InSource::NULL, 31);
        // Put result data from ISR to FIFO.
        a.push(false, true);
        // Start over.
        a.jmp(pio::JmpCondition::Always, &mut wrap_target);

        a.bind(&mut read_loop);
        // Shift-in 1-bit to ISR. and set SWCLK to High.
        a.r#in_with_delay_and_side_set(pio::InSource::PINS, 1, Q, HI);
        a.bind(&mut read_loop_enter);
        // Keep looping unless X register is 0. and set SWCLK to Low.
        a.jmp_with_delay_and_side_set(pio::JmpCondition::XDecNonZero, &mut read_loop, Q, LO);
        a.r#in_with_side_set(pio::InSource::PINS, 1, HI);
        // Put result data from ISR to FIFO.
        a.push(false, true);
        a.bind(&mut wrap_source);
        // Start over.
        // a.jmp(pio::JmpCondition::Always, &mut wrap_target);

        // The labels wrap_target and wrap_source, as set above,
        // define a loop which is executed repeatedly by the PIO
        // state machine.
        let program = a.assemble_with_wrap(wrap_source, wrap_target);
        // let program = a.assemble_program();
        // let program = program.set_origin(Some(0));

        // Initialize and start PIO
        let (mut pio, sm0, _, _, _) = pio0.split(resets);
        let installed = pio.install(&program).unwrap();
        // let div = 25f32; // 125MHz / 25 = 5Mhz
        // let div = 5f32; // 125MHz / 5 = 25Mhz
        let div = 1f32; // 125MHz / 1 = 125Mhz
        let (sm, rx, tx) = hal::pio::PIOBuilder::from_program(installed)
            .set_pins(clk_pin_id, 1)
            .side_set_pin_base(clk_pin_id)
            .out_pins(dat_pin_id, 1)
            .out_shift_direction(hal::pio::ShiftDirection::Right)
            .in_pin_base(dat_pin_id)
            .in_shift_direction(hal::pio::ShiftDirection::Right)
            .clock_divisor(div)
            .build(sm0);

        // Pin modes are controlled in connect() and disconnect() .
        // sm.set_pindirs([(clk_pin_id, hal::pio::PinDir::Output), (dat_pin_id, hal::pio::PinDir::Output)]);

        let running_sm = sm.start();

        Self {
            // clk_pin_id,
            // dat_pin_id,
            running_sm,
            rx_fifo: rx,
            tx_fifo: tx,
            _pins: core::marker::PhantomData,
        }
    }
}

// Auxiliary low-level function
impl<C, D> SwdIoSet<C, D> {
    fn set_clk_pindir(&mut self, oe: bool) {
        self.running_sm.exec_instruction(
            pio::InstructionOperands::SET {
                destination: pio::SetDestination::PINDIRS,
                data: match oe {
                    true => 1,
                    false => 0,
                },
            }
            .encode(),
        );
    }
}

// Connect and disconnect function
impl<C, D> SwdIoSet<C, D> {
    fn connect(&mut self) {
        self.set_clk_pindir(true);
        self.to_swdio_out(true);
    }
    fn disconnect(&mut self) {
        self.to_swdio_in();
        self.set_clk_pindir(false);
    }
}

// Basis of SWD interface
impl<C, D> SwdIoSet<C, D> {
    // if bits > 32 , it will behave as if value is extended with zeros.
    fn write_bits(&mut self, bits: u32, value: u32) {
        while !self.tx_fifo.write(bits | 1 << 31) {}
        while !self.tx_fifo.write(value) {}
    }
    // if bits > 32 , result is undefined.
    fn read_bits(&mut self, bits: u32) -> u32 {
        while !self.tx_fifo.write(bits | 0) {}
        while self.rx_fifo.is_empty() {}
        let value = unsafe { self.rx_fifo.read().unwrap_unchecked() };
        value >> ((32 - bits) & 31)
    }
}

// Supplemental functions for SWD
impl<C, D> SwdIoSet<C, D> {
    fn to_swdio_in(&mut self) {
        self.read_bits(0);
    }
    fn to_swdio_out(&mut self, output: bool) {
        self.write_bits(
            0,
            match output {
                true => 1,
                false => 0,
            },
        );
    }
    fn idle_cycle(&mut self, config: &SwdIoConfig) {
        if config.idle_cycles != 0 {
            self.write_bits(config.idle_cycles, 0);
        }
    }
}

/*
pub trait ConnectDisconnectSwdIo {
    fn connect(&mut self);
    fn disconnect(&mut self);
}

pub trait BasicSwdIo {
    fn write_bits(&mut self, bits: u32, value: u32);
    fn read_bits(&mut self, bits: u32) -> u32;
}

pub trait SupplementSwdIo {
    fn to_swdio_in(&mut self);
    fn to_swdio_out(&mut self, output: bool);
    fn idle_cycle(&mut self, config: &SwdIoConfig);
}
*/

impl<C, D> SwdIo for SwdIoSet<C, D> {
    fn connect(&mut self) {
        self.connect();
    }
    fn disconnect(&mut self) {
        self.disconnect();
    }
    fn swj_sequence(&mut self, config: &SwdIoConfig, count: usize, data: &[u8]) {
        self.swd_write_sequence(config, count, data);
    }
    fn swd_read_sequence(&mut self, _config: &SwdIoConfig, count: usize, data: &mut [u8]) {
        let mut count = count as u32;
        let mut index = 0;
        let mut bits = 0;
        let mut value = 0;
        while count != 0 || bits != 0 {
            if bits == 0 {
                bits = if count <= 32 { count } else { 32 };
                value = self.read_bits(bits);
                count -= bits;
            }
            data[index] = value as u8;
            index += 1;
            value >>= 8;
            bits -= if bits <= 8 { bits } else { 8 };
        }
    }
    fn swd_write_sequence(&mut self, _config: &SwdIoConfig, count: usize, data: &[u8]) {
        let mut count = count as u32;
        let mut index = 0;
        let mut bits = 0;
        let mut value = 0;
        while count != 0 {
            value |= (data[index] as u32) << bits;
            index += 1;
            bits += 8;
            if count <= bits {
                bits = count;
            }
            if bits == count || bits == 32 {
                self.write_bits(bits, value);
                count -= bits;
                bits = 0;
                value = 0;
            }
        }
    }
    fn swd_transfer(
        &mut self,
        config: &SwdIoConfig,
        request: SwdRequest,
        data: u32,
    ) -> core::result::Result<u32, DapError> {
        // READ or WIRTE operation
        // send request
        self.enable_output();
        {
            let mut bits = request.bits() & 0b1111;
            bits |= (bits.count_ones() as u8 & 1) << 4;
            bits = bits << 1 | 0x81;
            self.write_bits(8, bits as u32);
        }

        let ack;
        if request.contains(SwdRequest::RnW) {
            // READ operation
            self.disable_output();
            // turnaround + recv ack.
            ack = self.read_bits(3 + config.turn_around_cycles) as u8 >> config.turn_around_cycles
                & 0b111;
            if ack == DAP_TRANSFER_OK {
                // recv data
                let value = self.read_bits(32);
                let parity = value.count_ones() & 1;
                // recv parity + turnaround
                let parity_expected = self.read_bits(1 + config.turn_around_cycles) & 1;
                let result = match parity == parity_expected {
                    true => Ok(value),
                    false => Err(DapError::SwdError(DAP_TRANSFER_MISMATCH)),
                };
                // TODO: capture timestamp
                self.enable_output();
                self.idle_cycle(config);
                self.to_swdio_out(true);
                return result;
            }
        } else {
            // WRITE operation
            self.disable_output();
            // turnaround + read ack + turnaround.
            ack = self.read_bits(3 + config.turn_around_cycles * 2) as u8
                >> config.turn_around_cycles
                & 0b111;
            if ack == DAP_TRANSFER_OK {
                self.enable_output();
                // send data
                self.write_bits(32, data);
                // send parity
                self.write_bits(1, data.count_ones());
                // TODO: capture timestamp
                self.idle_cycle(config);
                self.to_swdio_out(true);
                return Ok(0);
            }
        }

        // An error occured.
        if ack == DAP_TRANSFER_WAIT || ack == DAP_TRANSFER_FAULT {
            self.disable_output();
            if config.always_generate_data_phase && request.contains(SwdRequest::RnW) {
                self.read_bits(33);
            }
            if request.contains(SwdRequest::RnW) {
                // turnaround
                self.read_bits(config.turn_around_cycles);
            }
            self.enable_output();
            if config.always_generate_data_phase && !request.contains(SwdRequest::RnW) {
                self.write_bits(33, 0);
            }
            self.to_swdio_out(true);
            return Err(DapError::SwdError(ack));
        }

        // Protocol error
        self.read_bits(33);
        if request.contains(SwdRequest::RnW) {
            // turnaround
            self.read_bits(config.turn_around_cycles);
        }
        self.enable_output();
        self.to_swdio_out(true);
        return Err(DapError::SwdError(ack));
    }

    fn enable_output(&mut self) {
        // Enabling of output is inherent in write_bits()
    }

    fn disable_output(&mut self) {
        // Disabling of output is inherent in read_bits()
    }
}
