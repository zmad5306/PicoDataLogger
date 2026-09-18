# Pico Data Logger

A data logger built with:

- Raspberry Pi Pico 2 W
- Adafruit SHT40 temperature and humidity sensor (I2C)
- Qwiic-compatible cable

## Hardware wiring

The SHT40 is connected to the Pico 2 W using the following Qwiic wire mapping. Physical pin numbers refer to the numbered pins on the Pico 2 W header.

| Qwiic wire | Signal | Pico 2 W pin | Physical pin number |
| --- | --- | --- | ---: |
| Red | Power | 3V3 OUT | 36 |
| Black | Ground | GND | 3 |
| Blue | SDA (data) | GP0 | 1 |
| Yellow | SCL (clock) | GP1 | 2 |

The I2C data and clock lines use GP0 and GP1, respectively.
