
use partitions_macro::{ partition_offset, partition };
use partitions_macro_types::Partition;


pub const NVS_OFFSET: u32 = partition_offset!("nvs");
pub const OTA_DATA_OFFSET: u32 = partition_offset!("otadata");
pub const OTA_0_OFFSET: u32 = partition_offset!("ota_0");
pub const OTA_1_OFFSET: u32 = partition_offset!("ota_1");
pub const NVS_PARTITION: Partition = partition!("nvs");
pub const OTA_DATA_PARTITION: Partition = partition!("otadata");
pub const OTA_0_PARTITION: Partition = partition!("ota_0");
pub const OTA_1_PARTITION: Partition = partition!("ota_1");