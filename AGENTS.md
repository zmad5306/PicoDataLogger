# AI Collaboration Contract

PicoDataLogger is a Rust and embedded-programming learning project. When implementing work, keep changes aligned with the active GitHub lesson issue and explain the Rust or hardware concepts introduced by the requested slice. Prefer small, observable steps over completing later lessons early.

A request to implement, continue, review, or check an issue authorizes work only within that issue or the explicitly named subset. Inspect the issue, its dependencies, the current repository, and relevant upstream documentation before editing. Do not treat a request for explanation or diagnosis as permission to change code.

Preserve the hardware configuration documented in `README.md`: the SHT40 uses 3V3, GND, GP0/SDA, and GP1/SCL. Do not claim an on-device checkpoint passed from compilation alone. Hardware-dependent completion requires logs or observations from the physical Pico, either gathered directly when available or reported by the user.

## GitHub lesson tracking

When the user asks to start, implement part of, continue, review, or report progress on a GitHub lesson issue, use the repository-local `pico-issue-tracking` skill.

Maintain one authoritative tracker comment per issue. Update only items supported by repository, test, build, or hardware evidence, and synchronize the matching checkboxes in the issue body so the learner-facing roadmap stays current. Preserve the issue's wording and structure. Keep learner reflection questions unchecked until the user answers them, and do not close an issue unless the user explicitly requests that action.
