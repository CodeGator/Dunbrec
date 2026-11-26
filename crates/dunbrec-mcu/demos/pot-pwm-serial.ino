// Pot, PWM and serial: reads a voltage, copies it to a PWM pin and prints it.
// To try it, connect a potentiometer to the analog pin chosen below, the
// oscilloscope to the PWM pin, and open the serial monitor.
// After a change, just press Run: Dunbrec builds the sketch first.

// ----------------------------------------------------------------------
// Settings: change these.

const int INPUT_PIN = A0;            // which analog pin is read: A0 to A5
const int PWM_PIN = 6;               // a PWM pin: 3, 5, 6, 9, 10 or 11
const unsigned long EVERY_MS = 100;  // wait between readings, in milliseconds
// ----------------------------------------------------------------------

void setup() {
  pinMode(PWM_PIN, OUTPUT);
  Serial.begin(9600);
  Serial.println("Dunbrec Arduino demo");
}

void loop() {
  int reading = analogRead(INPUT_PIN);   // 0 to 1023
  analogWrite(PWM_PIN, reading / 4);     // 0 to 255
  Serial.print("Reading = ");
  Serial.println(reading);
  delay(EVERY_MS);
}
