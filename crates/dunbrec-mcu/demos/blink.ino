// Blink: turns a light on and off.
// To see it, connect a logic LED to the pin chosen below.
// Press Build in the Code panel after every change, then Run.

// ----------------------------------------------------------------------
// Settings: change these.

const int LED_PIN = 13;              // which pin the light is on
const unsigned long ON_MS = 500;     // time on, in milliseconds
const unsigned long OFF_MS = 500;    // time off, in milliseconds
// ----------------------------------------------------------------------

void setup() {
  pinMode(LED_PIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_PIN, HIGH);
  delay(ON_MS);
  digitalWrite(LED_PIN, LOW);
  delay(OFF_MS);
}
