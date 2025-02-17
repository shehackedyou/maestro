/*
 * This file implements the function that handles the system calls.
 */

.include "src/process/regs/regs.s"

.section .text

.global syscall
.type syscall, @function

/*
 * The function handling system calls.
 */
syscall:
	cli
	push %ebp
	mov %esp, %ebp

	# Storing registers state
GET_REGS

	# Setting data segment
	mov $GDT_KERNEL_DS, %ax
	mov %ax, %ds
	mov %ax, %es

	# Calling the system call handler
	push %esp
	sti
	call syscall_handler
	cli
	add $4, %esp

	# Restoring data segment
	xor %ebx, %ebx
	mov $GDT_USER_DS, %bx
	or $3, %bx
	mov %bx, %ds
	mov %bx, %es

RESTORE_REGS

	# Restoring the context
	mov %ebp, %esp
	pop %ebp
	iret
