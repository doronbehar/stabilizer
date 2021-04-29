#![deny(warnings)]
#![no_std]
#![no_main]

use stabilizer::{hardware, net};

use miniconf::Miniconf;
use serde::Deserialize;

use dsp::iir;
use hardware::{
    Adc0Input, Adc1Input, AfeGain, Dac0Output, Dac1Output, DigitalInput0,
    DigitalInput1, InputPin, SystemTimer, AFE0, AFE1,
};

use net::{Action, MqttInterface};

const SCALE: f32 = i16::MAX as _;

// The number of cascaded IIR biquads per channel. Select 1 or 2!
const IIR_CASCADE_LENGTH: usize = 1;

#[derive(Clone, Copy, Debug, Deserialize, Miniconf)]
pub struct Settings {
    afe: [AfeGain; 2],
    iir_ch: [[iir::IIR; IIR_CASCADE_LENGTH]; 2],
    allow_hold: bool,
    force_hold: bool,
    telemetry_period_secs: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Analog frontend programmable gain amplifier gains (G1, G2, G5, G10)
            afe: [AfeGain::G1, AfeGain::G1],
            // IIR filter tap gains are an array `[b0, b1, b2, a1, a2]` such that the
            // new output is computed as `y0 = a1*y1 + a2*y2 + b0*x0 + b1*x1 + b2*x2`.
            // The array is `iir_state[channel-index][cascade-index][coeff-index]`.
            // The IIR coefficients can be mapped to other transfer function
            // representations, for example as described in https://arxiv.org/abs/1508.06319
            iir_ch: [[iir::IIR::new(1., -SCALE, SCALE); IIR_CASCADE_LENGTH]; 2],
            // Permit the DI1 digital input to suppress filter output updates.
            allow_hold: false,
            // Force suppress filter output updates.
            force_hold: false,
            telemetry_period_secs: 10,
        }
    }
}

#[rtic::app(device = stm32h7xx_hal::stm32, peripherals = true, monotonic = stabilizer::hardware::SystemTimer)]
const APP: () = {
    struct Resources {
        afes: (AFE0, AFE1),
        digital_inputs: (DigitalInput0, DigitalInput1),
        adcs: (Adc0Input, Adc1Input),
        dacs: (Dac0Output, Dac1Output),
        mqtt: MqttInterface<Settings>,
        telemetry: net::Telemetry,
        settings: Settings,

        #[init([[[0.; 5]; IIR_CASCADE_LENGTH]; 2])]
        iir_state: [[iir::Vec5; IIR_CASCADE_LENGTH]; 2],
    }

    #[init(spawn=[telemetry, settings_update])]
    fn init(c: init::Context) -> init::LateResources {
        // Configure the microcontroller
        let (mut stabilizer, _pounder) = hardware::setup(c.core, c.device);

        let mqtt = MqttInterface::new(
            stabilizer.net.stack,
            "",
            &net::get_device_prefix(
                env!("CARGO_BIN_NAME"),
                stabilizer.net.mac_address,
            ),
            stabilizer.net.phy,
            stabilizer.cycle_counter,
        );

        // Spawn a settings update for default settings.
        c.spawn.settings_update().unwrap();
        c.spawn.telemetry().unwrap();

        // Enable ADC/DAC events
        stabilizer.adcs.0.start();
        stabilizer.adcs.1.start();
        stabilizer.dacs.0.start();
        stabilizer.dacs.1.start();

        // Start sampling ADCs.
        stabilizer.adc_dac_timer.start();

        init::LateResources {
            afes: stabilizer.afes,
            adcs: stabilizer.adcs,
            dacs: stabilizer.dacs,
            mqtt,
            digital_inputs: stabilizer.digital_inputs,
            telemetry: net::Telemetry::default(),
            settings: Settings::default(),
        }
    }

    /// Main DSP processing routine for Stabilizer.
    ///
    /// # Note
    /// Processing time for the DSP application code is bounded by the following constraints:
    ///
    /// DSP application code starts after the ADC has generated a batch of samples and must be
    /// completed by the time the next batch of ADC samples has been acquired (plus the FIFO buffer
    /// time). If this constraint is not met, firmware will panic due to an ADC input overrun.
    ///
    /// The DSP application code must also fill out the next DAC output buffer in time such that the
    /// DAC can switch to it when it has completed the current buffer. If this constraint is not met
    /// it's possible that old DAC codes will be generated on the output and the output samples will
    /// be delayed by 1 batch.
    ///
    /// Because the ADC and DAC operate at the same rate, these two constraints actually implement
    /// the same time bounds, meeting one also means the other is also met.
    #[task(binds=DMA1_STR4, resources=[adcs, digital_inputs, dacs, iir_state, settings, telemetry], priority=2)]
    fn process(c: process::Context) {
        let adc_samples = [
            c.resources.adcs.0.acquire_buffer(),
            c.resources.adcs.1.acquire_buffer(),
        ];

        let dac_samples = [
            c.resources.dacs.0.acquire_buffer(),
            c.resources.dacs.1.acquire_buffer(),
        ];

        let hold = c.resources.settings.force_hold
            || (c.resources.digital_inputs.1.is_high().unwrap()
                && c.resources.settings.allow_hold);

        for channel in 0..adc_samples.len() {
            for sample in 0..adc_samples[0].len() {
                let mut y = f32::from(adc_samples[channel][sample] as i16);
                for i in 0..c.resources.iir_state[channel].len() {
                    y = c.resources.settings.iir_ch[channel][i].update(
                        &mut c.resources.iir_state[channel][i],
                        y,
                        hold,
                    );
                }
                // Note(unsafe): The filter limits ensure that the value is in range.
                // The truncation introduces 1/2 LSB distortion.
                let y = unsafe { y.to_int_unchecked::<i16>() };
                // Convert to DAC code
                dac_samples[channel][sample] = y as u16 ^ 0x8000;
            }
        }

        // Update telemetry measurements.
        // TODO: Should we report these as voltages?
        c.resources.telemetry.latest_samples =
            [adc_samples[0][0] as i16, adc_samples[1][0] as i16];

        c.resources.telemetry.latest_outputs =
            [dac_samples[0][0] as i16, dac_samples[1][0] as i16];

        c.resources.telemetry.digital_inputs = [
            c.resources.digital_inputs.0.is_high().unwrap(),
            c.resources.digital_inputs.1.is_high().unwrap(),
        ];
    }

    #[idle(resources=[mqtt], spawn=[settings_update])]
    fn idle(mut c: idle::Context) -> ! {
        loop {
            match c.resources.mqtt.lock(|mqtt| mqtt.update()) {
                Some(Action::Sleep) => cortex_m::asm::wfi(),
                Some(Action::UpdateSettings) => {
                    c.spawn.settings_update().unwrap()
                }
                _ => {}
            }
        }
    }

    #[task(priority = 1, resources=[mqtt, afes, settings])]
    fn settings_update(mut c: settings_update::Context) {
        // Update the IIR channels.
        let settings = c.resources.mqtt.settings();
        c.resources.settings.lock(|current| *current = settings);

        // Update AFEs
        c.resources.afes.0.set_gain(settings.afe[0]);
        c.resources.afes.1.set_gain(settings.afe[1]);
    }

    #[task(priority = 1, resources=[mqtt, settings, telemetry], schedule=[telemetry])]
    fn telemetry(mut c: telemetry::Context) {
        let telemetry =
            c.resources.telemetry.lock(|telemetry| telemetry.clone());

        c.resources.mqtt.publish_telemetry(&telemetry);

        let telemetry_period = c
            .resources
            .settings
            .lock(|settings| settings.telemetry_period_secs);

        // Schedule the telemetry task in the future.
        c.schedule
            .telemetry(
                c.scheduled
                    + SystemTimer::ticks_from_secs(telemetry_period as u32),
            )
            .unwrap();
    }

    #[task(binds = ETH, priority = 1)]
    fn eth(_: eth::Context) {
        unsafe { stm32h7xx_hal::ethernet::interrupt_handler() }
    }

    #[task(binds = SPI2, priority = 3)]
    fn spi2(_: spi2::Context) {
        panic!("ADC0 input overrun");
    }

    #[task(binds = SPI3, priority = 3)]
    fn spi3(_: spi3::Context) {
        panic!("ADC1 input overrun");
    }

    #[task(binds = SPI4, priority = 3)]
    fn spi4(_: spi4::Context) {
        panic!("DAC0 output error");
    }

    #[task(binds = SPI5, priority = 3)]
    fn spi5(_: spi5::Context) {
        panic!("DAC1 output error");
    }

    extern "C" {
        // hw interrupt handlers for RTIC to use for scheduling tasks
        // one per priority
        fn DCMI();
        fn JPEG();
        fn SDMMC();
    }
};
