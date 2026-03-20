# tg-rcore-tutorial-ch1-storage
`tg-rcore-tutorial-ch1-storage` 在 `tg-rcore-tutorial-ch1` 基础上扩展了最小磁盘读写能力。内核在 S-Mode 直接驱动 VirtIO 块设备，完成一个块写入、读回校验、再恢复原块数据。
