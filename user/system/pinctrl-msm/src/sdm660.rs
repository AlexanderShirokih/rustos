//! Выводы SDM660 по `drivers/pinctrl/qcom/pinctrl-sdm660.c` Linux 6.12.

use crate::registers::{PinGroup, Tile};

macro_rules! groups {
    (@name _) => {
        ""
    };
    (@name $function:ident) => {
        stringify!($function)
    };
    ($($tile:ident: $($function:tt),+;)*) => {
        &[$(PinGroup {
            tile: Tile::$tile,
            functions: [$(groups!(@name $function)),+],
        },)*]
    };
}

/// Выводы GPIO SDM660 в порядке номеров.
pub const SDM660: &[PinGroup] = groups! {
    South: blsp_spi1, blsp_uart1, blsp_uim1, tgu_ch0, _, _, qdss_gpio4, atest_gpsadc1, _;
    South: blsp_spi1, blsp_uart1, blsp_uim1, tgu_ch1, _, _, qdss_gpio5, atest_gpsadc0, _;
    South: blsp_spi1, blsp_uart1, blsp_i2c1, _, _, _, _, _, _;
    South: blsp_spi1, blsp_uart1, blsp_i2c1, ddr_bist, _, _, atest_tsens2, atest_usb1, _;
    North: blsp_spi2, blsp_uim2, blsp_uart2, phase_flag3, _, _, _, _, _;
    South: blsp_spi2, blsp_uim2, blsp_uart2, phase_flag14, _, _, _, _, _;
    South: blsp_spi2, blsp_i2c2, blsp_uart2, phase_flag31, _, _, _, _, _;
    South: blsp_spi2, blsp_i2c2, blsp_uart2, _, _, _, _, _, _;
    North: blsp_spi3, ddr_bist, _, _, _, wlan1_adc1, atest_usb13, bimc_dte1, _;
    North: blsp_spi3, ddr_bist, _, _, _, wlan1_adc0, atest_usb12, bimc_dte0, _;
    North: blsp_spi3, blsp_i2c3, ddr_bist, _, _, wlan2_adc1, atest_usb11, bimc_dte1, _;
    North: blsp_spi3, blsp_i2c3, _, dbg_out, wlan2_adc0, atest_usb10, bimc_dte0, _, _;
    North: blsp_spi4, pri_mi2s, _, phase_flag26, qdss_cti1_b, _, _, _, _;
    North: blsp_spi4, _, pri_mi2s_ws, _, _, phase_flag27, qdss_cti0_b, _, _;
    North: blsp_spi4, blsp_i2c4, pri_mi2s, _, phase_flag28, _, _, _, _;
    North: blsp_spi4, blsp_i2c4, pri_mi2s, _, _, _, _, _, _;
    Center: blsp_uart5, blsp_spi5, blsp_uim5, _, _, _, _, _, _;
    Center: blsp_uart5, blsp_spi5, blsp_uim5, _, phase_flag5, _, _, _, _;
    Center: blsp_uart5, blsp_spi5, blsp_i2c5, _, _, _, _, _, _;
    Center: blsp_uart5, blsp_spi5, blsp_i2c5, _, _, _, _, _, _;
    South: _, _, blsp_uim6, _, _, _, _, _, _;
    South: _, _, blsp_uim6, _, phase_flag11, qdss_cti0_b, vsense_data0, _, _;
    Center: blsp_spi6, _, blsp_i2c6, _, phase_flag12, vsense_data1, _, _, _;
    Center: blsp_spi6, _, blsp_i2c6, _, phase_flag13, vsense_mode, _, _, _;
    North: blsp_spi7, blsp_uart6_a, sec_mi2s, sndwire_clk, _, _, phase_flag17, vsense_clkout, _;
    North: blsp_spi7, blsp_uart6_a, sec_mi2s, sndwire_data, _, _, phase_flag18, _, _;
    North: blsp_spi7, blsp_uart6_a, blsp_i2c7, sec_mi2s, _, phase_flag19, _, _, _;
    North: blsp_spi7, blsp_uart6_a, blsp_i2c7, vfr_1, sec_mi2s, _, phase_flag20, _, _;
    Center: blsp_spi8_a, blsp_uart6_b, m_voc, _, phase_flag21, _, _, _, _;
    Center: blsp_spi8_a, blsp_uart6_b, _, _, phase_flag22, _, _, _, _;
    Center: blsp_spi8_a, blsp_uart6_b, blsp_i2c8_a, blsp_spi3_cs1, _, phase_flag23, _, _, _;
    Center: blsp_spi8_a, blsp_uart6_b, blsp_i2c8_a, pwr_modem, _, phase_flag24, qdss_gpio, _, _;
    South: cam_mclk, pwr_nav, _, _, qdss_gpio0, _, _, _, _;
    South: cam_mclk, qspi_data0, pwr_crypto, _, _, qdss_gpio1, _, _, _;
    South: cam_mclk, qspi_data1, agera_pll, _, _, qdss_gpio2, _, _, _;
    South: cam_mclk, qspi_data2, jitter_bist, _, _, qdss_gpio3, _, atest_usb2, _;
    South: cci_i2c, pll_bypassnl, agera_pll, _, _, qdss_gpio4, atest_tsens, atest_usb21, _;
    South: cci_i2c, pll_reset, _, _, qdss_gpio5, atest_usb23, _, _, _;
    South: cci_i2c, _, _, qdss_gpio6, _, _, _, _, _;
    South: cci_i2c, _, _, qdss_gpio7, _, _, _, _, _;
    South: _, _, blsp_spi8_b, _, _, _, _, _, _;
    South: _, _, blsp_spi8_b, _, _, _, _, _, _;
    South: mdss_vsync0, mdss_vsync1, mdss_vsync2, mdss_vsync3, _, _, qdss_gpio9, _, _;
    South: _, _, qspi_cs, _, _, qdss_gpio10, _, _, _;
    South: _, _, blsp_spi8_b, blsp_i2c8_b, _, _, qdss_gpio11, _, _;
    South: cci_async, _, _, qdss_gpio12, _, _, _, _, _;
    South: blsp_spi1, _, _, qdss_gpio13, _, _, _, _, _;
    South: qspi_clk, _, phase_flag30, qdss_gpio14, _, _, _, _, _;
    South: _, phase_flag1, qdss_gpio15, _, _, _, _, _, _;
    South: blsp_spi6, phase_flag2, qdss_cti0_a, _, _, _, _, _, _;
    South: qspi_cs, _, phase_flag9, qdss_cti0_a, _, _, _, _, _;
    South: qspi_data3, _, phase_flag15, qdss_gpio8, _, _, _, _, _;
    South: _, blsp_spi8_b, blsp_i2c8_b, blsp_spi6, phase_flag16, qdss_gpio, _, _, _;
    North: _, phase_flag6, qdss_cti1_a, _, _, _, _, _, _;
    North: _, _, phase_flag29, _, _, _, _, _, _;
    South: _, phase_flag25, qdss_cti1_a, _, _, _, _, _, _;
    South: _, phase_flag10, qdss_gpio3, _, atest_usb20, _, _, _, _;
    South: gcc_gp1, _, phase_flag4, atest_usb22, _, _, _, _, _;
    South: _, gcc_gp2, _, _, atest_char, _, _, _, _;
    North: mdp_vsync, gcc_gp3, _, _, atest_char3, _, _, _, _;
    North: cri_trng0, _, _, atest_char2, _, _, _, _, _;
    North: pri_mi2s, cri_trng1, _, _, atest_char1, _, _, _, _;
    North: sec_mi2s, audio_ref, _, cri_trng, _, _, atest_char0, _, _;
    North: _, _, _, qdss_gpio1, _, _, _, _, _;
    South: blsp_spi8_cs1, sp_cmu, _, _, qdss_gpio2, _, _, _, _;
    South: _, nav_pps_a, nav_pps_a, gps_tx_a, blsp_spi3_cs2, adsp_ext, _, _, _;
    North: _, _, qdss_cti1_b, _, _, _, _, _, _;
    North: _, _, qdss_gpio0, _, _, _, _, _, _;
    North: isense_dbg, _, phase_flag0, qdss_gpio, _, _, _, _, _;
    North: _, phase_flag7, qdss_gpio, _, _, _, _, _, _;
    North: _, phase_flag8, qdss_gpio6, _, _, _, _, _, _;
    North: _, _, qdss_gpio7, tsense_pwm1, tsense_pwm2, _, _, _, _;
    North: _, qdss_gpio14, _, _, _, _, _, _, _;
    North: _, _, qdss_gpio15, _, _, _, _, _, _;
    North: mdp_vsync, _, _, _, _, _, _, _, _;
    North: _, _, qdss_gpio8, _, _, _, _, _, _;
    North: blsp_spi8_cs2, _, _, _, qdss_gpio9, _, _, _, _;
    North: _, _, qdss_gpio10, _, _, _, _, _, _;
    North: gcc_gp1, _, qdss_gpio13, _, _, _, _, _, _;
    South: _, _, qdss_gpio11, _, _, _, _, _, _;
    South: nav_pps_b, nav_pps_b, gps_tx_c, _, _, qdss_gpio12, _, _, _;
    Center: mss_lte, gcc_gp2, _, _, _, _, _, _, _;
    Center: mss_lte, gcc_gp3, _, _, _, _, _, _, _;
    South: uim2_data, _, _, _, _, _, _, _, _;
    South: uim2_clk, _, _, _, _, _, _, _, _;
    South: uim2_reset, _, _, _, _, _, _, _, _;
    South: uim2_present, _, _, _, _, _, _, _, _;
    South: uim1_data, _, _, _, _, _, _, _, _;
    South: uim1_clk, _, _, _, _, _, _, _, _;
    South: uim1_reset, _, _, _, _, _, _, _, _;
    South: uim1_present, _, _, _, _, _, _, _, _;
    South: uim_batt, _, _, _, _, _, _, _, _;
    South: _, _, pa_indicator, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, ldo_en, _, _, _, _, _, _, _;
    South: _, nav_pps_c, nav_pps_c, gps_tx_b, ldo_update, _, _, _, _;
    South: qlink_request, _, _, _, _, _, _, _, _;
    South: qlink_enable, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, prng_rosc, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
    South: _, _, _, _, _, _, _, _, _;
};
