#![no_std]
#![no_main]
#![cfg_attr(target_arch = "riscv64", deny(warnings))]
#![cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    fmt::{self, Write},
    ptr::{addr_of_mut, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};
use tg_sbi::{console_putchar, shutdown};
use virtio_drivers::{Hal, MmioTransport, VirtIOBlk, VirtIOHeader};

// QEMU virt 平台 VirtIO-BLK MMIO 基址
const VIRTIO0: usize = 0x1000_1000;
// VirtIO 通用寄存器：中断状态/应答
const IRQ_STATUS: usize = VIRTIO0 + 0x60;
const IRQ_ACK: usize = VIRTIO0 + 0x64;
// 本实验读写的目标扇区号
const BLOCK_ID: usize = 8;
const PAGE_SIZE: usize = 4096;
// 给 VirtIO DMA 申请的页数
const DMA_PAGES: usize = 32;
// 给 virtio-drivers 运行时分配使用的最小堆
const HEAP_SIZE: usize = 256 * 1024;

#[repr(C, align(4096))]
struct DmaArea([u8; PAGE_SIZE * DMA_PAGES]);

static mut DMA_AREA: DmaArea = DmaArea([0u8; PAGE_SIZE * DMA_PAGES]);
static DMA_NEXT: AtomicUsize = AtomicUsize::new(0);

#[repr(C, align(16))]
struct HeapArea([u8; HEAP_SIZE]);

struct BumpAllocator {
    // 堆基址在运行时初始化，避免 static mut 引用告警
    base: UnsafeCell<usize>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // 无锁 bump 分配：仅前移 next，不支持回收
        let base = unsafe { *self.base.get() };
        let align_mask = layout.align() - 1;
        let size = layout.size();
        loop {
            let old = self.next.load(Ordering::SeqCst);
            let start = (old + align_mask) & !align_mask;
            let end = start.saturating_add(size);
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange(old, end, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return (base + start) as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

static mut HEAP_AREA: HeapArea = HeapArea([0u8; HEAP_SIZE]);

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    base: UnsafeCell::new(0),
    next: AtomicUsize::new(0),
};

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // 复用 SBI 控制台输出日志，便于观察测试结果
        for b in s.bytes() {
            console_putchar(b);
        }
        Ok(())
    }
}

fn print(args: fmt::Arguments) {
    let _ = Console.write_fmt(args);
}

macro_rules! println {
    () => { print(format_args!("\n")) };
    ($($arg:tt)*) => {{
        print(format_args!($($arg)*));
        print(format_args!("\n"));
    }};
}

struct VirtioHal;

impl Hal for VirtioHal {
    fn dma_alloc(pages: usize) -> usize {
        // 为 virtio 队列分配连续物理页（本实验采用恒等映射）
        let start = DMA_NEXT.fetch_add(pages, Ordering::SeqCst);
        if start + pages > DMA_PAGES {
            panic!();
        }
        let base = unsafe { addr_of_mut!(DMA_AREA.0) as *mut u8 as usize };
        base + start * PAGE_SIZE
    }

    fn dma_dealloc(_paddr: usize, _pages: usize) -> i32 {
        0
    }

    fn phys_to_virt(paddr: usize) -> usize {
        // ch1 场景使用恒等映射
        paddr
    }

    fn virt_to_phys(vaddr: usize) -> usize {
        vaddr
    }
}

fn ack_interrupt() -> u32 {
    // 读取设备中断状态并按位回写应答
    let status = unsafe { (IRQ_STATUS as *const u32).read_volatile() };
    if status != 0 {
        unsafe { (IRQ_ACK as *mut u32).write_volatile(status) };
    }
    status
}

fn storage_demo() -> bool {
    // 初始化 VirtIO 块设备传输层
    let mut blk = unsafe {
        VirtIOBlk::<VirtioHal, MmioTransport>::new(
            MmioTransport::new(NonNull::new(VIRTIO0 as *mut VirtIOHeader).unwrap()).unwrap(),
        )
        .unwrap()
    };

    let mut backup = [0u8; 512];
    // 先备份原扇区，避免污染镜像
    blk.read_block(BLOCK_ID, &mut backup).unwrap();

    let mut write_buf = [0u8; 512];
    let payload = b"TG-CH1-STORAGE-DEMO";
    write_buf[..payload.len()].copy_from_slice(payload);

    blk.write_block(BLOCK_ID, &write_buf).unwrap();
    let irq_after_write = ack_interrupt();

    let mut read_buf = [0u8; 512];
    blk.read_block(BLOCK_ID, &mut read_buf).unwrap();
    let irq_after_read = ack_interrupt();

    blk.write_block(BLOCK_ID, &backup).unwrap();

    println!("irq_after_write={}", irq_after_write);
    println!("irq_after_read={}", irq_after_read);

    read_buf == write_buf
}

#[cfg(target_arch = "riscv64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
unsafe extern "C" fn _start() -> ! {
    const STACK_SIZE: usize = 4096;
    #[unsafe(link_section = ".bss.uninit")]
    static mut STACK: [u8; STACK_SIZE] = [0u8; STACK_SIZE];
    core::arch::naked_asm!(
        "la sp, {stack} + {stack_size}",
        "j  {main}",
        stack_size = const STACK_SIZE,
        stack      =   sym STACK,
        main       =   sym rust_main,
    )
}

extern "C" fn rust_main() -> ! {
    unsafe {
        // 运行时初始化全局分配器基址
        *ALLOCATOR.base.get() = addr_of_mut!(HEAP_AREA.0) as *mut u8 as usize;
    }
    println!("ch1-storage: start");
    if storage_demo() {
        println!("STORAGE-OK");
        shutdown(false)
    } else {
        println!("STORAGE-FAIL");
        shutdown(true)
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("panic");
    shutdown(true)
}

#[cfg(not(target_arch = "riscv64"))]
mod stub {
    #[unsafe(no_mangle)]
    pub extern "C" fn main() -> i32 {
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn __libc_start_main() -> i32 {
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rust_eh_personality() {}
}
