; Pot, PWM and serial: reads a voltage, copies it to a PWM pin and prints it.
; To try it, connect a potentiometer to the analog pin chosen below, the
; oscilloscope to the PWM pin, open the serial monitor and press Run.

; ----------------------------------------------------------------------
; Settings: change these, then press Run.

; Which analog pin is read: 0 to 5, for A0 to A5.
        .equ    INPUT = 0

; Which pin gets the PWM. Change all three lines together.
;   D6: PWM_BIT = 6, PWM_OCR = OCR0A, PWM_MODE = 0x83
;   D5: PWM_BIT = 5, PWM_OCR = OCR0B, PWM_MODE = 0x23
        .equ    PWM_BIT  = 6
        .equ    PWM_OCR  = OCR0A
        .equ    PWM_MODE = 0x83

; Milliseconds to wait between readings (1 to 65535).
        .equ    EVERY_MS = 100
; ----------------------------------------------------------------------

        ldi     r16, high(RAMEND)       ; set up the stack, for rcall
        out     SPH, r16
        ldi     r16, low(RAMEND)
        out     SPL, r16

        sbi     DDRD, PWM_BIT           ; the PWM pin is an output
        ldi     r16, PWM_MODE
        out     TCCR0A, r16
        ldi     r16, 0x03
        out     TCCR0B, r16
        ; Analog input: measured against the supply, result left adjusted.
        ldi     r16, 0x60 | INPUT
        sts     ADMUX, r16
        ldi     r16, 0x87               ; converter on, slow clock
        sts     ADCSRA, r16
        ; Serial: 9600 baud, transmitter only, 8 data bits.
        ldi     r16, 0
        sts     UBRR0H, r16
        ldi     r16, 103
        sts     UBRR0L, r16
        ldi     r16, 0x08
        sts     UCSR0B, r16
        ldi     r16, 0x06
        sts     UCSR0C, r16

        ldi     r30, low(banner * 2)
        ldi     r31, high(banner * 2)
        rcall   print

main:   lds     r16, ADCSRA             ; start a measurement
        ori     r16, 0x40
        sts     ADCSRA, r16
wait:   lds     r16, ADCSRA             ; and wait for it
        sbrc    r16, 6
        rjmp    wait
        lds     r24, ADCL
        lds     r25, ADCH
        out     PWM_OCR, r25            ; the top 8 bits set the PWM

        ; r23:r22 = the 10-bit result: ADCH * 4 + ADCL / 64.
        swap    r24
        lsr     r24
        lsr     r24
        andi    r24, 0x03
        mov     r22, r25
        clr     r23
        lsl     r22
        rol     r23
        lsl     r22
        rol     r23
        or      r22, r24

        ldi     r30, low(label * 2)
        ldi     r31, high(label * 2)
        rcall   print
        rcall   print_number
        ldi     r30, low(newline * 2)
        ldi     r31, high(newline * 2)
        rcall   print

        ldi     r24, low(EVERY_MS)
        ldi     r25, high(EVERY_MS)
        rcall   delay_ms
        rjmp    main

; Send the text at Z, which ends with a zero.
print:  lpm     r16, Z+
        tst     r16
        breq    printed
        rcall   send
        rjmp    print
printed:
        ret

; Send the character in r16.
send:   lds     r17, UCSR0A
        sbrs    r17, 5                  ; wait until there is room
        rjmp    send
        sts     UDR0, r16
        ret

; Send r23:r22 (0 to 9999) as four digits.
print_number:
        ldi     r20, low(1000)
        ldi     r21, high(1000)
        rcall   digit
        ldi     r20, 100
        ldi     r21, 0
        rcall   digit
        ldi     r20, 10
        ldi     r21, 0
        rcall   digit
        mov     r16, r22
        subi    r16, -'0'
        rjmp    send
digit:  ldi     r16, '0'
count:  cp      r22, r20
        cpc     r23, r21
        brlo    counted
        sub     r22, r20
        sbc     r23, r21
        inc     r16
        rjmp    count
counted:
        rjmp    send

; Wait r25:r24 milliseconds at 16 MHz.
delay_ms:
        ldi     r26, low(3999)          ; 4000 turns of 4 clock cycles
        ldi     r27, high(3999)         ; make one millisecond
inner:  sbiw    r26, 1
        brne    inner
        sbiw    r24, 1
        brne    delay_ms
        ret

; The texts. Change them to print something else.
banner: .db     "Dunbrec AVR demo", 13, 10, 0
label:  .db     'A', '0' + INPUT, " = ", 0
newline:
        .db     13, 10, 0
