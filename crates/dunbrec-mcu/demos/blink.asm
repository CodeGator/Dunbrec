; Blink: turns a light on and off.
; To see it, connect a logic LED to the pin chosen below and press Run.

; ----------------------------------------------------------------------
; Settings: change these, then press Run.

; Which pin the light is on.
;   Pins D8 to D13: use DDRB and PINB; the bit is the pin number minus 8.
;   Pins D0 to D7:  use DDRD and PIND; the bit is the pin number.
        .equ    LED_DDR  = DDRB
        .equ    LED_PINS = PINB
        .equ    LED_BIT  = 5            ; D13

; How long the light stays on, and off, in milliseconds (1 to 65535).
        .equ    ON_MS  = 500
        .equ    OFF_MS = 500
; ----------------------------------------------------------------------

        ldi     r16, high(RAMEND)       ; set up the stack, for rcall
        out     SPH, r16
        ldi     r16, low(RAMEND)
        out     SPL, r16

        sbi     LED_DDR, LED_BIT        ; the pin is an output

loop:   sbi     LED_PINS, LED_BIT       ; writing a 1 here flips the pin: on
        ldi     r24, low(ON_MS)
        ldi     r25, high(ON_MS)
        rcall   delay_ms
        sbi     LED_PINS, LED_BIT       ; off
        ldi     r24, low(OFF_MS)
        ldi     r25, high(OFF_MS)
        rcall   delay_ms
        rjmp    loop

; Wait r25:r24 milliseconds at 16 MHz.
delay_ms:
        ldi     r26, low(3999)          ; 4000 turns of 4 clock cycles
        ldi     r27, high(3999)         ; make one millisecond
inner:  sbiw    r26, 1
        brne    inner
        sbiw    r24, 1
        brne    delay_ms
        ret
