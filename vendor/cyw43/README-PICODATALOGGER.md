# PicoDataLogger CYW43 patch

This directory vendors `cyw43` 0.7.0 from Embassy. PicoDataLogger adds one
method, `Control::join_with_gpio_flash`, so the Pico 2 W onboard LED can keep
flashing while the driver's Wi-Fi association event loop is pending.

The upstream `Control::join` method exclusively borrows `Control`; application
code therefore cannot call `gpio_set` concurrently. The added method performs
the same join and services a timer inside `wait_for_join`, using the existing
control owner and runner. The original `join` API and behavior remain intact.
