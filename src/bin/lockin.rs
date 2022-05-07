//! # Lockin
//!
//! THe `lockin` application implements a lock-in amplifier using either an external or internally
//! generated
//!
//! ## Features
//! * Up to 800 kHz sampling
//! * Up to 400 kHz modulation frequency
//! * Supports internal and external reference sources:
//!     1. Internal: Generate reference internally and output on one of the channel outputs
//!     2. External: Reciprocal PLL, reference input applied to DI0.
//! * Adjustable PLL and locking time constants
//! * Adjustable phase offset and harmonic index
//! * Run-time configurable output modes (in-phase, quadrature, magnitude, log2 power, phase, frequency)
//! * Input/output data streamng via UDP
//!
//! ## Settings
//! Refer to the [Settings] structure for documentation of run-time configurable settings for this
//! application.
//!
//! ## Telemetry
//! Refer to [Telemetry] for information about telemetry reported by this application.
//!
//! ## Livestreaming
//! This application streams raw ADC and DAC data over UDP. Refer to
//! [stabilizer::net::data_stream](../stabilizer/net/data_stream/index.html) for more information.
#![deny(warnings)]
#![no_std]
#![no_main]

use core::{
    convert::TryFrom,
    sync::atomic::{fence, Ordering},
};

use mutex_trait::prelude::*;

use idsp::{Accu, Complex, ComplexExt, Lockin, RPLL};
use rtic::time::duration::Extensions;
use stabilizer::{
    hardware::{
        self,
        adc::{Adc0Input, Adc1Input, AdcCode},
        afe::Gain,
        dac::{Dac0Output, Dac1Output, DacCode},
        embedded_hal::digital::v2::InputPin,
        hal,
        input_stamper::InputStamper,
        signal_generator,
        system_timer::SystemTimer,
        DigitalInput0, DigitalInput1, AFE0, AFE1,
    },
    net::{
        data_stream::{FrameGenerator, StreamFormat, StreamTarget},
        miniconf::Miniconf,
        serde::Deserialize,
        telemetry::{Telemetry, TelemetryBuffer},
        NetworkState, NetworkUsers,
    },
};

// The logarithm of the number of samples in each batch process. This corresponds with 2^3 samples
// per batch = 8 samples
const BATCH_SIZE_SIZE_LOG2: u8 = 3;

// The logarithm of the number of 100MHz timer ticks between each sample. This corresponds with a
// sampling period of 2^7 = 128 ticks. At 100MHz, 10ns per tick, this corresponds to a sampling
// period of 1.28 uS or 781.25 KHz.
const ADC_SAMPLE_TICKS_LOG2: u8 = 7;

// Copied from ./dual-iir.rs
// The logarithm of the number of 100MHz timer ticks between each sample. With a value of 2^7 =
// 128, there is 1.28uS per sample, corresponding to a sampling frequency of 781.25 KHz.
const SAMPLE_TICKS_LOG2: u8 = 7;

#[derive(Copy, Clone, Debug, Deserialize, Miniconf)]
enum Conf {
    /// Output the lockin magnitude.
    Magnitude,
    /// Output the phase of the lockin
    Phase,
    /// Output the lockin reference frequency as a sinusoid
    ReferenceFrequency,
    /// Output the logarithmic power of the lockin
    LogPower,
    /// Output the in-phase component of the lockin signal.
    InPhase,
    /// Output the quadrature component of the lockin signal.
    Quadrature,
    /// Output the lockin internal modulation frequency as a sinusoid
    Modulation,
}

#[derive(Copy, Clone, Debug, Miniconf, Deserialize, PartialEq)]
enum LockinMode {
    /// Utilize an internally generated reference for demodulation
    Internal,
    /// Utilize an external modulation signal supplied to DI0
    External,
}

#[derive(Copy, Clone, Debug, Deserialize, Miniconf)]
pub struct Settings {
    /// Configure the Analog Front End (AFE) gain.
    ///
    /// # Path
    /// `afe/<n>`
    ///
    /// * <n> specifies which channel to configure. <n> := [0, 1]
    ///
    /// # Value
    /// Any of the variants of [Gain] enclosed in double quotes.
    afe: [Gain; 2],

    /// Specifies the operational mode of the lockin.
    ///
    /// # Path
    /// `lockin_mode`
    ///
    /// # Value
    /// One of the variants of [LockinMode] enclosed in double quotes.
    lockin_mode: LockinMode,

    /// Specifis the PLL time constant.
    ///
    /// # Path
    /// `pll_tc/<n>`
    ///
    /// * <n> specifies which channel to configure. <n> := [0, 1]
    ///
    /// # Value
    /// The PLL time constant as an unsigned byte (0-255).
    pll_tc: [u8; 2],

    /// Specifies the lockin time constant.
    ///
    /// # Path
    /// `lockin_tc`
    ///
    /// # Value
    /// The lockin low-pass time constant as an unsigned byte (0-255).
    lockin_tc: u8,

    /// Specifies which harmonic to use for the lockin.
    ///
    /// # Path
    /// `lockin_harmonic`
    ///
    /// # Value
    /// Harmonic index of the LO. -1 to _de_modulate the fundamental (complex conjugate)
    lockin_harmonic: i32,

    /// Specifies the LO phase offset.
    ///
    /// # Path
    /// `lockin_phase`
    ///
    /// # Value
    /// Demodulation LO phase offset. Units are in terms of i32, where [i32::MIN] is equivalent to
    /// -pi and [i32::MAX] is equivalent to +pi.
    lockin_phase: i32,

    /// Specifies DAC output mode.
    ///
    /// # Path
    /// `output_conf/<n>`
    ///
    /// * <n> specifies which channel to configure. <n> := [0, 1]
    ///
    /// # Value
    /// One of the variants of [Conf] enclosed in double quotes.
    output_conf: [Conf; 2],

    /// Specifies the telemetry output period in seconds.
    ///
    /// # Path
    /// `telemetry_period`
    ///
    /// # Value
    /// Any non-zero value less than 65536.
    telemetry_period: u16,

    /// Specifies the target for data livestreaming.
    ///
    /// # Path
    /// `stream_target`
    ///
    /// # Value
    /// See [StreamTarget#miniconf]
    stream_target: StreamTarget,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            afe: [Gain::G1; 2],

            lockin_mode: LockinMode::External,

            pll_tc: [21, 21], // frequency and phase settling time (log2 counter cycles)

            lockin_tc: 6,        // lockin lowpass time constant
            lockin_harmonic: -1, // Harmonic index of the LO: -1 to _de_modulate the fundamental (complex conjugate)
            lockin_phase: 0,     // Demodulation LO phase offset

            output_conf: [Conf::InPhase, Conf::Quadrature],
            // The default telemetry period in seconds.
            telemetry_period: 10,

            stream_target: StreamTarget::default(),
        }
    }
}

#[rtic::app(device = stabilizer::hardware::hal::stm32, peripherals = true, dispatchers=[DCMI, JPEG, SDMMC])]
mod app {
    use super::*;

    #[monotonic(binds = TIM15)]
    type Monotonic = SystemTimer;

    #[shared]
    struct Shared {
        network: NetworkUsers<Settings, Telemetry>,
        settings: Settings,
        telemetry: TelemetryBuffer,
    }

    #[local]
    struct Local {
        afes: (AFE0, AFE1),
        generator: FrameGenerator,
        digital_inputs: (DigitalInput0, DigitalInput1),
        adcs: (Adc0Input, Adc1Input),
        dacs: (Dac0Output, Dac1Output),
        signal_generator: signal_generator::SignalGenerator,

        timestamper: InputStamper,
        pll: RPLL,
        lockin: Lockin<4>,
    }

    #[init]
    fn init(c: init::Context) -> (Shared, Local, init::Monotonics) {
        // Configure the microcontroller
        let (mut stabilizer, _pounder) = hardware::setup::setup(
            c.core,
            c.device,
            1 << BATCH_SIZE_SIZE_LOG2,
            1 << ADC_SAMPLE_TICKS_LOG2,
        );

        let mut network = NetworkUsers::new(
            stabilizer.net.stack,
            stabilizer.net.phy,
            env!("CARGO_BIN_NAME"),
            stabilizer.net.mac_address,
            option_env!("BROKER")
                .unwrap_or("10.34.16.10")
                .parse()
                .unwrap(),
        );

        let generator = network.configure_streaming(
            StreamFormat::AdcDacData,
            1u8 << BATCH_SIZE_SIZE_LOG2,
        );

        let settings = Settings::default();

        let pll = RPLL::new(ADC_SAMPLE_TICKS_LOG2 + BATCH_SIZE_SIZE_LOG2);

        // Spawn a settings and telemetry update for default settings.
        settings_update::spawn().unwrap();
        telemetry::spawn().unwrap();
        ethernet_link::spawn().unwrap();

        // Enable ADC/DAC events
        stabilizer.adcs.0.start();
        stabilizer.adcs.1.start();
        stabilizer.dacs.0.start();
        stabilizer.dacs.1.start();

        // Start recording digital input timestamps.
        stabilizer.timestamp_timer.start();

        // Start sampling ADCs.
        stabilizer.adc_dac_timer.start();

        // Enable the timestamper.
        stabilizer.timestamper.start();

        let signal_config = {
            signal_generator::BasicConfig {
                // ~0.1V Amplitude
                amplitude: DacCode::try_from(0.1).unwrap().into(),
                // A sort of generalization for "duty cycle".
                symmetry: 0.5,
                // It seems the limit is `2.0e5` - 200Khz. Above it the card shows a red LED that
                // seem to indicate an internal error. Probably the NYQUIST check at
                // ../hardware/signal_generator.rs fails and the error is indicated this way. Note
                // however that even using 2.0e5, seems to generate a not very clean signal.
                frequency: 1.0e4,

                signal: signal_generator::Signal::Cosine,
            }
        };

        let shared = Shared {
            network,
            telemetry: TelemetryBuffer::default(),

            settings,
        };

        let local = Local {
            digital_inputs: stabilizer.digital_inputs,
            afes: stabilizer.afes,
            adcs: stabilizer.adcs,
            dacs: stabilizer.dacs,
            timestamper: stabilizer.timestamper,

            signal_generator: signal_generator::SignalGenerator::new(
                signal_config.try_into_config(SAMPLE_TICKS_LOG2)
                .unwrap()
            ),

            pll,
            generator,
            lockin: Lockin::default(),
        };

        (shared, local, init::Monotonics(SystemTimer::default()))
    }

    /// Main DSP processing routine.
    ///
    /// See `dual-iir` for general notes on processing time and timing.
    ///
    /// This is an implementation of a externally (DI0) referenced PLL lockin on the ADC0 signal.
    /// It outputs either I/Q or power/phase on DAC0/DAC1. Data is normalized to full scale.
    /// PLL bandwidth, filter bandwidth, slope, and x/y or power/phase post-filters are available.
    #[task(binds=DMA1_STR4, shared=[settings, telemetry], local=[adcs, dacs, lockin, timestamper, pll, generator, signal_generator], priority=2)]
    #[link_section = ".itcm.process"]
    fn process(c: process::Context) {
        let process::LocalResources {
            adcs: (ref mut adc0, ref mut adc1),
            dacs: (ref mut dac0, ref mut dac1),
            lockin,
            pll,
            timestamper,
            generator,
            signal_generator,
        } = c.local;

        let process::SharedResources {
            settings,
            telemetry,
        } = c.shared;

        (settings, telemetry).lock(|settings, telemetry| {
            let (reference_phase, reference_frequency) = match settings
                .lockin_mode
            {
                LockinMode::External => {
                    let timestamp =
                        timestamper.latest_timestamp().unwrap_or(None); // Ignore data from timer capture overflows.
                    let (pll_phase, pll_frequency) = pll.update(
                        timestamp.map(|t| t as i32),
                        settings.pll_tc[0],
                        settings.pll_tc[1],
                    );
                    (pll_phase, (pll_frequency >> BATCH_SIZE_SIZE_LOG2) as i32)
                }
                LockinMode::Internal => {
                    // Reference phase and frequency are known.
                    (1i32 << 30, 1i32 << (32 - BATCH_SIZE_SIZE_LOG2))
                }
            };

            let sample_frequency =
                reference_frequency.wrapping_mul(settings.lockin_harmonic);
            let sample_phase = settings.lockin_phase.wrapping_add(
                reference_phase.wrapping_mul(settings.lockin_harmonic),
            );

            (adc0, adc1, dac0, dac1).lock(|adc0, adc1, dac0, dac1| {
                let adc_samples = [adc0, adc1];
                let mut dac_samples = [dac0, dac1];

                // Preserve instruction and data ordering w.r.t. DMA flag access.
                fence(Ordering::SeqCst);

                let output: Complex<i32> = adc_samples[0]
                    .iter()
                    // Zip in the LO phase.
                    .zip(Accu::new(sample_phase, sample_frequency))
                    // Convert to signed, MSB align the ADC sample, update the Lockin (demodulate, filter)
                    .map(|(&sample, phase)| {
                        let s = (sample as i16 as i32) << 16;
                        lockin.update(s, phase, settings.lockin_tc)
                    })
                    // Decimate
                    .last()
                    .unwrap()
                    * 2; // Full scale assuming the 2f component is gone.

                // Convert to DAC data.
                for (channel, samples) in dac_samples.iter_mut().enumerate() {
                    for sample in samples.iter_mut() {
                        let value = match settings.output_conf[channel] {
                            Conf::Magnitude => output.abs_sqr() as i32 >> 16,
                            Conf::Phase => output.arg() >> 16,
                            Conf::LogPower => {
                                (output.log2() << 24) as i32 >> 16
                            }
                            Conf::ReferenceFrequency => {
                                reference_frequency as i32 >> 16
                            }
                            Conf::InPhase => output.re >> 16,
                            Conf::Quadrature => output.im >> 16,

                            Conf::Modulation => {
                                signal_generator.next().unwrap() as i32
                            }
                        };

                        *sample = DacCode::from(value as i16).0;
                    }
                }

                // Stream the data.
                const N: usize =
                    (1 << BATCH_SIZE_SIZE_LOG2) * core::mem::size_of::<u16>();
                generator.add::<_, { N * 4 }>(|buf| {
                    for (data, buf) in adc_samples
                        .iter()
                        .chain(dac_samples.iter())
                        .zip(buf.chunks_exact_mut(N))
                    {
                        let data = unsafe {
                            core::slice::from_raw_parts(
                                data.as_ptr() as *const u8,
                                N,
                            )
                        };
                        buf.copy_from_slice(data)
                    }
                });

                // Update telemetry measurements.
                telemetry.adcs =
                    [AdcCode(adc_samples[0][0]), AdcCode(adc_samples[1][0])];

                telemetry.dacs =
                    [DacCode(dac_samples[0][0]), DacCode(dac_samples[1][0])];

                // Preserve instruction and data ordering w.r.t. DMA flag access.
                fence(Ordering::SeqCst);
            });
        });
    }

    #[idle(shared=[network])]
    fn idle(mut c: idle::Context) -> ! {
        loop {
            match c.shared.network.lock(|net| net.update()) {
                NetworkState::SettingsChanged => {
                    settings_update::spawn().unwrap()
                }
                NetworkState::Updated => {}
                NetworkState::NoChange => cortex_m::asm::wfi(),
            }
        }
    }

    #[task(priority = 1, local=[afes], shared=[network, settings])]
    fn settings_update(mut c: settings_update::Context) {
        let settings = c.shared.network.lock(|net| *net.miniconf.settings());

        c.local.afes.0.set_gain(settings.afe[0]);
        c.local.afes.1.set_gain(settings.afe[1]);

        c.shared.settings.lock(|current| *current = settings);

        let target = settings.stream_target.into();
        c.shared.network.lock(|net| net.direct_stream(target));
    }

    #[task(priority = 1, local=[digital_inputs], shared=[network, settings, telemetry])]
    fn telemetry(mut c: telemetry::Context) {
        let mut telemetry: TelemetryBuffer =
            c.shared.telemetry.lock(|telemetry| *telemetry);

        telemetry.digital_inputs = [
            c.local.digital_inputs.0.is_high().unwrap(),
            c.local.digital_inputs.1.is_high().unwrap(),
        ];

        let (gains, telemetry_period) = c
            .shared
            .settings
            .lock(|settings| (settings.afe, settings.telemetry_period));

        c.shared.network.lock(|net| {
            net.telemetry
                .publish(&telemetry.finalize(gains[0], gains[1]))
        });

        // Schedule the telemetry task in the future.
        telemetry::Monotonic::spawn_after((telemetry_period as u32).seconds())
            .unwrap();
    }

    #[task(priority = 1, shared=[network])]
    fn ethernet_link(mut c: ethernet_link::Context) {
        c.shared.network.lock(|net| net.processor.handle_link());
        ethernet_link::Monotonic::spawn_after(1u32.seconds()).unwrap();
    }

    #[task(binds = ETH, priority = 1)]
    fn eth(_: eth::Context) {
        unsafe { hal::ethernet::interrupt_handler() }
    }
}
