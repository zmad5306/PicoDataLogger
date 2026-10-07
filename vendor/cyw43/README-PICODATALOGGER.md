# PicoDataLogger CYW43 patch

This directory vendors `cyw43` 0.7.0 from Embassy. PicoDataLogger adds
`Control::gpio_get` for USB-power sensing and `Control::join_with_gpio_flash`, so the Pico 2 W onboard LED can keep
flashing while the driver's Wi-Fi association event loop is pending.

The upstream `Control::join` method exclusively borrows `Control`; application
code therefore cannot call `gpio_set` concurrently. The added method performs
the same join and services a timer inside `wait_for_join`, using the existing
control owner and runner. The original `join` API and behavior remain intact.

`Control::gpio_get` reads the `ccgpioin` IOVAR and tests bit `gpio_n`, following [the upstream CYW43 driver](https://github.com/georgerobotics/cyw43-driver/blob/main/src/cyw43_ll.c). It never drives the input pin and returns `None` for invalid pins or short responses. The application bounds this read with a timeout. On Pico 2 W, WL_GPIO2 is high when USB VBUS is present.
