// Fade: raises and lowers the average voltage on a pin, using PWM.
// To see it, connect the oscilloscope to the pin chosen below: the pulses
// grow wider, then narrower.
// Press Build in the Code panel after every change, then Run.

// ----------------------------------------------------------------------
// Settings: change these.

const int PWM_PIN = 6;               // a PWM pin: 3, 5, 6, 9, 10 or 11
const unsigned long STEP_MS = 4;     // time between steps; bigger is slower
const int STEP = 1;                  // how big each step is (1 to 255)
// ----------------------------------------------------------------------

void setup() {
  pinMode(PWM_PIN, OUTPUT);
}

void loop() {
  for (int level = 0; level < 255; level += STEP) {
    analogWrite(PWM_PIN, level);
    delay(STEP_MS);
  }
  for (int level = 255; level > 0; level -= STEP) {
    analogWrite(PWM_PIN, level);
    delay(STEP_MS);
  }
}
