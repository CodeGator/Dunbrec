; Fade: raises and lowers the average voltage on a pin, using PWM.
; To see it, connect the oscilloscope to the pin chosen below and press Run:
; the pulses grow wider, then narrower.

; ----------------------------------------------------------------------
; Settings: change these, then press Run.

; Which pin fades. Change all three lines together.
;   D6: PWM_BIT = 6, PWM_OCR = OCR0A, PWM_MODE = 0x83
;   D5: PWM_BIT = 5, PWM_OCR = OCR0B, PWM_MODE = 0x23
        .equ    PWM_BIT  = 6
        .equ    PWM_OCR  = OCR0A
        .equ    PWM_MODE = 0x83

; Milliseconds between brightness steps (1 to 65535). Bigger is slower:
; one fade up and down takes about 510 steps.
        .equ    STEP_MS = 4
; ----------------------------------------------------------------------

        ldi     r16, high(RAMEND)       ; set up the stack, for rcall
        out     SPH, r16
        ldi     r16, low(RAMEND)
        out     SPL, r16

        sbi     DDRD, PWM_BIT           ; the pin is an output
        ldi     r16, PWM_MODE           ; timer 0 drives the pin with PWM
        out     TCCR0A, r16
        ldi     r16, 0x03               ; at about 977 pulses a second
        out     TCCR0B, r16

        clr     r17                     ; r17 = how much of the time it is high
up:     out     PWM_OCR, r17
        rcall   step_delay
        inc     r17
        cpi     r17, 255
        brne    up
down:   out     PWM_OCR, r17
        rcall   step_delay
        dec     r17
        brne    down
        rjmp    up

step_delay:
        ldi     r24, low(STEP_MS)
        ldi     r25, high(STEP_MS)
; Wait r25:r24 milliseconds at 16 MHz.
delay_ms:
        ldi     r26, low(3999)          ; 4000 turns of 4 clock cycles
        ldi     r27, high(3999)         ; make one millisecond
inner:  sbiw    r26, 1
        brne    inner
        sbiw    r24, 1
        brne    delay_ms
        ret
