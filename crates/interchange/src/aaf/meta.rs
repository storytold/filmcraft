//! The meta-dictionary: the class model a written file uses. The AAF Object Specification v1.1
//! (§5) requires it: "An AAF file shall contain the class model being used by the file in the
//! MetaDictionary object's ClassDefinitions and TypeDefinitions properties." Readers that build
//! their object model from the file (LibAAF) refuse a file whose sets are empty.
//!
//! [`CLASSES`] holds every class the writer uses and its ancestors, each with the properties the
//! writer sets on it; [`TYPES`] holds every type those properties use and the types those are
//! made of. Labels, local tags, parents, optionality and type structure are copied from the SMPTE
//! registers (Groups ST 395, Elements ST 335, Types ST 2003 and Labels ST 400, as published at
//! registry.smpte-ra.org on 2026-11-02); names are the AAF names the registers give.
//! `tests::the_meta_dictionary_holds_the_class_model_of_the_file` checks that every property
//! written is defined and that every reference in the model resolves.

use super::ids::{META_DICTIONARY, class, meta, path, pid, ul};
use super::store::{Auid, Obj, Value, utf16z};

/// A class definition: name, class id, parent class id, concrete, the properties it defines.
pub(crate) struct Class {
    pub name: &'static str,
    pub id: Auid,
    pub parent: Auid,
    pub concrete: bool,
    pub props: &'static [Prop],
}

/// Whether instances must have a property, and whether it is their unique identifier.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Use {
    Req,
    Opt,
    /// Required, and the object's unique identification.
    Key,
}
use Use::*;

/// A property definition: name, local tag, label, type.
pub(crate) struct Prop {
    pub name: &'static str,
    pub pid: u16,
    pub id: Auid,
    pub ty: Auid,
    pub usage: Use,
}

/// A type definition.
pub(crate) struct Type {
    pub name: &'static str,
    pub id: Auid,
    pub kind: Kind,
}

/// What a type is made of (AAF Object Specification v1.1 §27.5–27.19).
pub(crate) enum Kind {
    /// Size in bytes, signed.
    Integer(u8, bool),
    Character,
    /// Element type.
    String(Auid),
    Rename(Auid),
    Record(&'static [(&'static str, Auid)]),
    FixedArray(Auid, u32),
    VariableArray(Auid),
    Set(Auid),
    /// Referenced class.
    StrongReference(Auid),
    /// Referenced class, and the labels of the path to the set of targets: the Header class, then
    /// the properties down to the set.
    WeakReference(Auid, &'static [Auid]),
    /// Element type, names and values.
    Enumeration(Auid, &'static [(&'static str, i64)]),
    ExtendibleEnumeration(&'static [(&'static str, Auid)]),
    Indirect,
    Stream,
}
use Kind::*;

/// A SMPTE label written as one number, as in its URN (`urn:smpte:ul:060e2b34.0101…`).
const fn u(label: u128) -> Auid {
    ul(label.to_be_bytes())
}

const fn c(name: &'static str, id: u8, parent: u8, concrete: bool, props: &'static [Prop]) -> Class {
    Class { name, id: class(id), parent: class(parent), concrete, props }
}

const fn p(name: &'static str, pid: u16, id: u128, ty: Auid, usage: Use) -> Prop {
    Prop { name, pid, id: u(id), ty, usage }
}

const fn t(name: &'static str, id: Auid, kind: Kind) -> Type {
    Type { name, id, kind }
}

/// Every class the writer uses and its ancestors, ancestors first: name, class (`class(xx)`),
/// parent, concrete, and the properties the writer sets that the class defines.
#[rustfmt::skip]
pub(crate) const CLASSES: &[Class] = &[
    c("InterchangeObject", 0x01, 0x01, false, &[]),
    c("EssenceDescriptor", 0x24, 0x01, false, &[
        p("Locator", 0x2F01, 0x060E2B34_01010102_06010104_06030000, T_LOCATOR_STRONG_REFERENCE_VECTOR, Opt),
    ]),
    c("FileDescriptor", 0x25, 0x24, false, &[
        p("SampleRate", 0x3001, 0x060E2B34_01010101_04060101_00000000, T_RATIONAL, Opt),
        p("Length", 0x3002, 0x060E2B34_01010101_04060102_00000000, T_LENGTH_TYPE, Opt),
        p("ContainerFormat", 0x3004, 0x060E2B34_01010102_06010104_01020000, T_CONTAINER_DEFINITION_WEAK_REFERENCE, Opt),
    ]),
    c("DigitalImageDescriptor", 0x27, 0x25, false, &[
        p("StoredHeight", 0x3202, 0x060E2B34_01010101_04010502_01000000, T_UINT32, Req),
        p("StoredWidth", 0x3203, 0x060E2B34_01010101_04010502_02000000, T_UINT32, Req),
        p("FrameLayout", 0x320C, 0x060E2B34_01010101_04010301_04000000, T_LAYOUT_TYPE, Req),
        p("VideoLineMap", 0x320D, 0x060E2B34_01010102_04010302_05000000, T_INT32_ARRAY, Req),
        p("ImageAspectRatio", 0x320E, 0x060E2B34_01010101_04010101_01000000, T_RATIONAL, Req),
    ]),
    c("CDCIDescriptor", 0x28, 0x27, true, &[
        p("ComponentWidth", 0x3301, 0x060E2B34_01010102_04010503_0A000000, T_UINT32, Req),
        p("HorizontalSubsampling", 0x3302, 0x060E2B34_01010101_04010501_05000000, T_UINT32, Req),
    ]),
    c("Component", 0x02, 0x01, false, &[
        p("DataDefinition", 0x0201, 0x060E2B34_01010102_04070100_00000000, T_DATA_DEFINITION_WEAK_REFERENCE, Req),
        p("Length", 0x0202, 0x060E2B34_01010102_07020201_01030000, T_LENGTH_TYPE, Opt),
        p("UserComments", 0x0204, 0x060E2B34_01010107_03020102_16000000, T_TAGGED_VALUE_STRONG_REFERENCE_VECTOR, Opt),
    ]),
    c("Segment", 0x03, 0x02, false, &[]),
    c("Event", 0x06, 0x03, false, &[
        p("Position", 0x0601, 0x060E2B34_01010102_07020103_03030000, T_POSITION_TYPE, Opt),
        p("Comment", 0x0602, 0x060E2B34_01010102_05300404_01000000, T_STRING, Opt),
    ]),
    c("CommentMarker", 0x08, 0x06, true, &[]),
    c("Mob", 0x34, 0x01, false, &[
        p("MobID", 0x4401, 0x060E2B34_01010101_01011510_00000000, T_MOB_ID_TYPE, Key),
        p("Name", 0x4402, 0x060E2B34_01010101_01030302_01000000, T_STRING, Opt),
        p("Slots", 0x4403, 0x060E2B34_01010102_06010104_06050000, T_TRACK_STRONG_REFERENCE_VECTOR, Req),
        p("LastModified", 0x4404, 0x060E2B34_01010102_07020110_02050000, T_TIME_STAMP, Req),
        p("CreationTime", 0x4405, 0x060E2B34_01010102_07020110_01030000, T_TIME_STAMP, Req),
        p("UserComments", 0x4406, 0x060E2B34_01010102_03020102_0C000000, T_TAGGED_VALUE_STRONG_REFERENCE_VECTOR, Opt),
        p("UsageCode", 0x4408, 0x060E2B34_01010107_05010108_00000000, T_USAGE_TYPE, Opt),
    ]),
    c("CompositionMob", 0x35, 0x34, true, &[]),
    c("Parameter", 0x3C, 0x01, false, &[
        p("Definition", 0x4C01, 0x060E2B34_01010102_06010104_01040000, T_AUID, Req),
    ]),
    c("ConstantValue", 0x3D, 0x3C, true, &[
        p("Value", 0x4D01, 0x060E2B34_01010102_05300507_00000000, T_INDIRECT, Req),
    ]),
    c("DefinitionObject", 0x1A, 0x01, false, &[
        p("Identification", 0x1B01, 0x060E2B34_01010102_01011503_00000000, T_AUID, Key),
        p("Name", 0x1B02, 0x060E2B34_01010102_01070102_03010000, T_STRING, Req),
        p("Description", 0x1B03, 0x060E2B34_01010102_03020301_02010000, T_STRING, Opt),
    ]),
    c("ContainerDefinition", 0x20, 0x1A, true, &[]),
    c("ContentStorage", 0x18, 0x01, true, &[
        p("Mobs", 0x1901, 0x060E2B34_01010102_06010104_05010000, T_PACKAGE_STRONG_REFERENCE_SET, Req),
        p("EssenceData", 0x1902, 0x060E2B34_01010102_06010104_05020000, T_ESSENCE_DATA_STRONG_REFERENCE_SET, Opt),
    ]),
    c("ControlPoint", 0x19, 0x01, true, &[
        p("Value", 0x1A02, 0x060E2B34_01010102_0530050D_00000000, T_INDIRECT, Req),
        p("Time", 0x1A03, 0x060E2B34_01010102_07020103_10020100, T_RATIONAL, Req),
        p("EditHint", 0x1A04, 0x060E2B34_01010102_05300508_00000000, T_EDIT_HINT_TYPE, Opt),
    ]),
    c("DataDefinition", 0x1B, 0x1A, true, &[]),
    c("Dictionary", 0x22, 0x01, true, &[
        p("OperationDefinitions", 0x2603, 0x060E2B34_01010102_06010104_05030000, T_OPERATION_DEFINITION_STRONG_REFERENCE_SET, Opt),
        p("ParameterDefinitions", 0x2604, 0x060E2B34_01010102_06010104_05040000, T_PARAMETER_DEFINITION_STRONG_REFERENCE_SET, Opt),
        p("DataDefinitions", 0x2605, 0x060E2B34_01010102_06010104_05050000, T_DATA_DEFINITION_STRONG_REFERENCE_SET, Opt),
        p("ContainerDefinitions", 0x2608, 0x060E2B34_01010102_06010104_05080000, T_CONTAINER_DEFINITION_STRONG_REFERENCE_SET, Opt),
        p("InterpolationDefinitions", 0x2609, 0x060E2B34_01010102_06010104_05090000, T_INTERPOLATION_DEFINITION_STRONG_REFERENCE_SET, Opt),
    ]),
    c("EssenceData", 0x23, 0x01, true, &[
        p("MobID", 0x2701, 0x060E2B34_01010102_06010106_01000000, T_MOB_ID_TYPE, Key),
        p("Data", 0x2702, 0x060E2B34_01010102_04070200_00000000, T_STREAM, Opt),
    ]),
    c("MobSlot", 0x38, 0x01, false, &[
        p("SlotID", 0x4801, 0x060E2B34_01010102_01070101_00000000, T_UINT32, Req),
        p("SlotName", 0x4802, 0x060E2B34_01010102_01070102_01000000, T_STRING, Opt),
        p("Segment", 0x4803, 0x060E2B34_01010102_06010104_02040000, T_SEGMENT_STRONG_REFERENCE, Req),
        p("PhysicalTrackNumber", 0x4804, 0x060E2B34_01010102_01040103_00000000, T_UINT32, Opt),
    ]),
    c("EventMobSlot", 0x39, 0x38, true, &[
        p("EditRate", 0x4901, 0x060E2B34_01010102_05300402_00000000, T_RATIONAL, Req),
    ]),
    c("Filler", 0x09, 0x03, true, &[]),
    c("Header", 0x2F, 0x01, true, &[
        p("ByteOrder", 0x3B01, 0x060E2B34_01010101_03010201_02000000, T_INT16, Opt),
        p("LastModified", 0x3B02, 0x060E2B34_01010102_07020110_02040000, T_TIME_STAMP, Req),
        p("Content", 0x3B03, 0x060E2B34_01010102_06010104_02010000, T_CONTENT_STORAGE_STRONG_REFERENCE, Req),
        p("Dictionary", 0x3B04, 0x060E2B34_01010102_06010104_02020000, T_DICTIONARY_STRONG_REFERENCE, Opt),
        p("Version", 0x3B05, 0x060E2B34_01010102_03010201_05000000, T_VERSION_TYPE, Req),
        p("IdentificationList", 0x3B06, 0x060E2B34_01010102_06010104_06040000, T_IDENTIFICATION_STRONG_REFERENCE_VECTOR, Req),
        p("ObjectModelVersion", 0x3B07, 0x060E2B34_01010102_03010201_04000000, T_UINT32, Opt),
        p("OperationalPattern", 0x3B09, 0x060E2B34_01010105_01020203_00000000, T_AUID, Req),
    ]),
    c("Identification", 0x30, 0x01, true, &[
        p("CompanyName", 0x3C01, 0x060E2B34_01010102_05200701_02010000, T_STRING, Req),
        p("ProductName", 0x3C02, 0x060E2B34_01010102_05200701_03010000, T_STRING, Req),
        p("ProductVersionString", 0x3C04, 0x060E2B34_01010102_05200701_05010000, T_STRING, Req),
        p("ProductID", 0x3C05, 0x060E2B34_01010102_05200701_07000000, T_AUID, Req),
        p("Date", 0x3C06, 0x060E2B34_01010102_07020110_02030000, T_TIME_STAMP, Opt),
        p("Platform", 0x3C08, 0x060E2B34_01010102_05200701_06010000, T_STRING, Opt),
        p("GenerationAUID", 0x3C09, 0x060E2B34_01010102_05200701_01000000, T_AUID, Req),
    ]),
    c("InterpolationDefinition", 0x21, 0x1A, true, &[]),
    c("MasterMob", 0x36, 0x34, true, &[]),
    c("Locator", 0x31, 0x01, false, &[]),
    c("NetworkLocator", 0x32, 0x31, true, &[
        p("URLString", 0x4001, 0x060E2B34_01010101_01020101_01000000, T_STRING, Req),
    ]),
    c("OperationDefinition", 0x1C, 0x1A, true, &[
        p("DataDefinition", 0x1E01, 0x060E2B34_01010102_05300509_00000000, T_DATA_DEFINITION_WEAK_REFERENCE, Req),
        p("IsTimeWarp", 0x1E02, 0x060E2B34_01010101_05300503_00000000, T_BOOLEAN, Opt),
        p("NumberInputs", 0x1E07, 0x060E2B34_01010101_05300504_00000000, T_INT32, Req),
        p("ParametersDefined", 0x1E09, 0x060E2B34_01010102_06010104_03020000, T_PARAMETER_DEFINITION_WEAK_REFERENCE_SET, Opt),
    ]),
    c("OperationGroup", 0x0A, 0x03, true, &[
        p("Operation", 0x0B01, 0x060E2B34_01010102_05300506_00000000, T_OPERATION_DEFINITION_WEAK_REFERENCE, Req),
        p("InputSegments", 0x0B02, 0x060E2B34_01010102_06010104_06020000, T_SEGMENT_STRONG_REFERENCE_VECTOR, Opt),
        p("Parameters", 0x0B03, 0x060E2B34_01010102_06010104_060A0000, T_PARAMETER_STRONG_REFERENCE_VECTOR, Opt),
    ]),
    c("ParameterDefinition", 0x1D, 0x1A, true, &[
        p("DisplayUnits", 0x1F03, 0x060E2B34_01010102_0530050B_01000000, T_STRING, Opt),
    ]),
    c("SoundDescriptor", 0x42, 0x25, true, &[
        p("QuantizationBits", 0x3D01, 0x060E2B34_01010104_04020303_04000000, T_UINT32, Req),
        p("Locked", 0x3D02, 0x060E2B34_01010104_04020301_04000000, T_BOOLEAN, Opt),
        p("AudioSamplingRate", 0x3D03, 0x060E2B34_01010105_04020301_01010000, T_RATIONAL, Req),
        p("Channels", 0x3D07, 0x060E2B34_01010105_04020101_04000000, T_UINT32, Req),
    ]),
    c("PCMDescriptor", 0x48, 0x42, true, &[
        p("AverageBPS", 0x3D09, 0x060E2B34_01010105_04020303_05000000, T_UINT32, Req),
        p("BlockAlign", 0x3D0A, 0x060E2B34_01010105_04020302_01000000, T_UINT16, Req),
    ]),
    c("Sequence", 0x0F, 0x03, true, &[
        p("Components", 0x1001, 0x060E2B34_01010102_06010104_06090000, T_COMPONENT_STRONG_REFERENCE_VECTOR, Req),
    ]),
    c("SourceReference", 0x10, 0x03, false, &[
        p("SourceID", 0x1101, 0x060E2B34_01010102_06010103_01000000, T_MOB_ID_TYPE, Opt),
        p("SourceMobSlotID", 0x1102, 0x060E2B34_01010102_06010103_02000000, T_UINT32, Req),
    ]),
    c("SourceClip", 0x11, 0x10, true, &[
        p("StartTime", 0x1201, 0x060E2B34_01010102_07020103_01040000, T_POSITION_TYPE, Opt),
    ]),
    c("SourceMob", 0x37, 0x34, true, &[
        p("EssenceDescription", 0x4701, 0x060E2B34_01010102_06010104_02030000, T_ESSENCE_DESCRIPTOR_STRONG_REFERENCE, Req),
    ]),
    c("TaggedValue", 0x3F, 0x01, true, &[
        p("Name", 0x5001, 0x060E2B34_01010102_03020102_09010000, T_STRING, Req),
        p("Value", 0x5003, 0x060E2B34_01010102_03020102_0A010000, T_INDIRECT, Req),
    ]),
    c("TapeDescriptor", 0x2E, 0x24, true, &[]),
    c("Timecode", 0x14, 0x03, true, &[
        p("Start", 0x1501, 0x060E2B34_01010102_07020103_01050000, T_POSITION_TYPE, Req),
        p("FPS", 0x1502, 0x060E2B34_01010102_04040101_02060000, T_UINT16, Req),
        p("Drop", 0x1503, 0x060E2B34_01010101_04040101_05000000, T_BOOLEAN, Req),
    ]),
    c("TimelineMobSlot", 0x3B, 0x38, true, &[
        p("EditRate", 0x4B01, 0x060E2B34_01010102_05300405_00000000, T_RATIONAL, Req),
        p("Origin", 0x4B02, 0x060E2B34_01010102_07020103_01030000, T_POSITION_TYPE, Req),
    ]),
    c("Transition", 0x17, 0x02, true, &[
        p("OperationGroup", 0x1801, 0x060E2B34_01010102_06010104_02050000, T_OPERATION_GROUP_STRONG_REFERENCE, Req),
        p("CutPoint", 0x1802, 0x060E2B34_01010102_07020103_01060000, T_POSITION_TYPE, Req),
    ]),
    c("VaryingValue", 0x3E, 0x3C, true, &[
        p("Interpolation", 0x4E01, 0x060E2B34_01010102_06010104_01050000, T_INTERPOLATION_DEFINITION_WEAK_REFERENCE, Req),
        p("PointList", 0x4E02, 0x060E2B34_01010102_06010104_06060000, T_CONTROL_POINT_STRONG_REFERENCE_VECTOR, Req),
    ]),
];

// Types, by their SMPTE labels.
const T_INT64: Auid = u(0x060E2B34_01040101_01010800_00000000);
const T_UINT8: Auid = u(0x060E2B34_01040101_01010100_00000000);
const T_UINT32: Auid = u(0x060E2B34_01040101_01010300_00000000);
const T_UINT16: Auid = u(0x060E2B34_01040101_01010200_00000000);
const T_INT32: Auid = u(0x060E2B34_01040101_01010700_00000000);
const T_INT16: Auid = u(0x060E2B34_01040101_01010600_00000000);
const T_INT8: Auid = u(0x060E2B34_01040101_01010500_00000000);
const T_CHARACTER: Auid = u(0x060E2B34_01040101_01100100_00000000);
const T_STRING: Auid = u(0x060E2B34_01040101_01100200_00000000);
const T_LENGTH_TYPE: Auid = u(0x060E2B34_01040101_01012002_00000000);
const T_POSITION_TYPE: Auid = u(0x060E2B34_01040101_01012001_00000000);
const T_AUID: Auid = u(0x060E2B34_01040101_01030100_00000000);
const T_MOB_ID_TYPE: Auid = u(0x060E2B34_01040101_01030200_00000000);
const T_RATIONAL: Auid = u(0x060E2B34_01040101_03010100_00000000);
const T_DATE_STRUCT: Auid = u(0x060E2B34_01040101_03010500_00000000);
const T_TIME_STRUCT: Auid = u(0x060E2B34_01040101_03010600_00000000);
const T_TIME_STAMP: Auid = u(0x060E2B34_01040101_03010700_00000000);
const T_VERSION_TYPE: Auid = u(0x060E2B34_01040101_03010300_00000000);
const T_UINT8_ARRAY12: Auid = u(0x060E2B34_01040101_04010200_00000000);
const T_UINT8_ARRAY8: Auid = u(0x060E2B34_01040101_04010800_00000000);
const T_INT32_ARRAY: Auid = u(0x060E2B34_01040101_04010300_00000000);
const T_BOOLEAN: Auid = u(0x060E2B34_01040101_01040100_00000000);
const T_EDIT_HINT_TYPE: Auid = u(0x060E2B34_01040101_02010106_00000000);
const T_LAYOUT_TYPE: Auid = u(0x060E2B34_01040101_02010108_00000000);
const T_USAGE_TYPE: Auid = u(0x060E2B34_01040101_02020104_00000000);
const T_INDIRECT: Auid = u(0x060E2B34_01040101_04100300_00000000);
const T_STREAM: Auid = u(0x060E2B34_01040101_04100200_00000000);
const T_TAGGED_VALUE_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021A00_00000000);
const T_SEGMENT_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020600_00000000);
const T_PARAMETER_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021600_00000000);
const T_COMPONENT_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020B00_00000000);
const T_OPERATION_GROUP_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020500_00000000);
const T_PACKAGE_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021300_00000000);
const T_ESSENCE_DATA_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020F00_00000000);
const T_OPERATION_DEFINITION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021500_00000000);
const T_PARAMETER_DEFINITION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021700_00000000);
const T_DATA_DEFINITION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020E00_00000000);
const T_CONTAINER_DEFINITION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020C00_00000000);
const T_INTERPOLATION_DEFINITION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021100_00000000);
const T_LOCATOR_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021200_00000000);
const T_CONTENT_STORAGE_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020100_00000000);
const T_DICTIONARY_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020200_00000000);
const T_IDENTIFICATION_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021000_00000000);
const T_TRACK_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05021400_00000000);
const T_ESSENCE_DESCRIPTOR_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020300_00000000);
const T_CONTROL_POINT_STRONG_REFERENCE: Auid = u(0x060E2B34_01040101_05020D00_00000000);
const T_DATA_DEFINITION_WEAK_REFERENCE: Auid = u(0x060E2B34_01040101_05010300_00000000);
const T_OPERATION_DEFINITION_WEAK_REFERENCE: Auid = u(0x060E2B34_01040101_05010700_00000000);
const T_PARAMETER_DEFINITION_WEAK_REFERENCE: Auid = u(0x060E2B34_01040101_05010800_00000000);
const T_CONTAINER_DEFINITION_WEAK_REFERENCE: Auid = u(0x060E2B34_01040101_05010200_00000000);
const T_INTERPOLATION_DEFINITION_WEAK_REFERENCE: Auid = u(0x060E2B34_01040101_05010500_00000000);
const T_TAGGED_VALUE_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060800_00000000);
const T_SEGMENT_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060600_00000000);
const T_PARAMETER_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060A00_00000000);
const T_COMPONENT_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060100_00000000);
const T_PACKAGE_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050700_00000000);
const T_ESSENCE_DATA_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050500_00000000);
const T_PARAMETER_DEFINITION_WEAK_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05030E00_00000000);
const T_OPERATION_DEFINITION_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050800_00000000);
const T_PARAMETER_DEFINITION_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050900_00000000);
const T_DATA_DEFINITION_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050400_00000000);
const T_CONTAINER_DEFINITION_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050300_00000000);
const T_INTERPOLATION_DEFINITION_STRONG_REFERENCE_SET: Auid = u(0x060E2B34_01040101_05050600_00000000);
const T_LOCATOR_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060400_00000000);
const T_IDENTIFICATION_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060300_00000000);
const T_TRACK_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060500_00000000);
const T_CONTROL_POINT_STRONG_REFERENCE_VECTOR: Auid = u(0x060E2B34_01040101_05060200_00000000);

/// Every type the properties above use, and the types those are made of.
#[rustfmt::skip]
pub(crate) const TYPES: &[Type] = &[
    t("Int64", T_INT64, Integer(8, true)),
    t("UInt8", T_UINT8, Integer(1, false)),
    t("UInt32", T_UINT32, Integer(4, false)),
    t("UInt16", T_UINT16, Integer(2, false)),
    t("Int32", T_INT32, Integer(4, true)),
    t("Int16", T_INT16, Integer(2, true)),
    t("Int8", T_INT8, Integer(1, true)),
    t("Character", T_CHARACTER, Character),
    t("String", T_STRING, String(T_CHARACTER)),
    t("LengthType", T_LENGTH_TYPE, Rename(T_INT64)),
    t("PositionType", T_POSITION_TYPE, Rename(T_INT64)),
    t("AUID", T_AUID, Record(&[("Data1", T_UINT32), ("Data2", T_UINT16), ("Data3", T_UINT16), ("Data4", T_UINT8_ARRAY8)])),
    t("MobIDType", T_MOB_ID_TYPE, Record(&[("SMPTELabel", T_UINT8_ARRAY12), ("Length", T_UINT8), ("InstanceHigh", T_UINT8), ("InstanceMid", T_UINT8), ("InstanceLow", T_UINT8), ("Material", T_AUID)])),
    t("Rational", T_RATIONAL, Record(&[("Numerator", T_INT32), ("Denominator", T_INT32)])),
    t("DateStruct", T_DATE_STRUCT, Record(&[("Year", T_INT16), ("Month", T_UINT8), ("Day", T_UINT8)])),
    t("TimeStruct", T_TIME_STRUCT, Record(&[("Hour", T_UINT8), ("Minute", T_UINT8), ("Second", T_UINT8), ("Fraction", T_UINT8)])),
    t("TimeStamp", T_TIME_STAMP, Record(&[("Date", T_DATE_STRUCT), ("Time", T_TIME_STRUCT)])),
    t("VersionType", T_VERSION_TYPE, Record(&[("VersionMajor", T_INT8), ("VersionMinor", T_INT8)])),
    t("UInt8Array12", T_UINT8_ARRAY12, FixedArray(T_UINT8, 12)),
    t("UInt8Array8", T_UINT8_ARRAY8, FixedArray(T_UINT8, 8)),
    t("Int32Array", T_INT32_ARRAY, VariableArray(T_INT32)),
    t("Boolean", T_BOOLEAN, Enumeration(T_UINT8, &[("False", 0), ("True", 1)])),
    t("EditHintType", T_EDIT_HINT_TYPE, Enumeration(T_UINT8, &[("NoEditHint", 0), ("Proportional", 1), ("RelativeLeft", 2), ("RelativeRight", 3), ("RelativeFixed", 4)])),
    t("LayoutType", T_LAYOUT_TYPE, Enumeration(T_UINT8, &[("FullFrame", 0), ("SeparateFields", 1), ("OneField", 2), ("MixedFields", 3), ("SegmentedFrame", 4)])),
    t("UsageType", T_USAGE_TYPE, ExtendibleEnumeration(&[
        ("Usage_SubClip", u(0x060E2B34_04010101_0D010102_01010500)),
        ("Usage_AdjustedClip", u(0x060E2B34_04010101_0D010102_01010600)),
        ("Usage_TopLevel", u(0x060E2B34_04010101_0D010102_01010700)),
        ("Usage_LowerLevel", u(0x060E2B34_04010101_0D010102_01010800)),
        ("Usage_Template", u(0x060E2B34_04010101_0D010102_01010900)),
    ])),
    t("Indirect", T_INDIRECT, Indirect),
    t("Stream", T_STREAM, Stream),
    t("TaggedValueStrongReference", T_TAGGED_VALUE_STRONG_REFERENCE, StrongReference(class(0x3F))),
    t("SegmentStrongReference", T_SEGMENT_STRONG_REFERENCE, StrongReference(class(0x03))),
    t("ParameterStrongReference", T_PARAMETER_STRONG_REFERENCE, StrongReference(class(0x3C))),
    t("ComponentStrongReference", T_COMPONENT_STRONG_REFERENCE, StrongReference(class(0x02))),
    t("OperationGroupStrongReference", T_OPERATION_GROUP_STRONG_REFERENCE, StrongReference(class(0x0A))),
    t("PackageStrongReference", T_PACKAGE_STRONG_REFERENCE, StrongReference(class(0x34))),
    t("EssenceDataStrongReference", T_ESSENCE_DATA_STRONG_REFERENCE, StrongReference(class(0x23))),
    t("OperationDefinitionStrongReference", T_OPERATION_DEFINITION_STRONG_REFERENCE, StrongReference(class(0x1C))),
    t("ParameterDefinitionStrongReference", T_PARAMETER_DEFINITION_STRONG_REFERENCE, StrongReference(class(0x1D))),
    t("DataDefinitionStrongReference", T_DATA_DEFINITION_STRONG_REFERENCE, StrongReference(class(0x1B))),
    t("ContainerDefinitionStrongReference", T_CONTAINER_DEFINITION_STRONG_REFERENCE, StrongReference(class(0x20))),
    t("InterpolationDefinitionStrongReference", T_INTERPOLATION_DEFINITION_STRONG_REFERENCE, StrongReference(class(0x21))),
    t("LocatorStrongReference", T_LOCATOR_STRONG_REFERENCE, StrongReference(class(0x31))),
    t("ContentStorageStrongReference", T_CONTENT_STORAGE_STRONG_REFERENCE, StrongReference(class(0x18))),
    t("DictionaryStrongReference", T_DICTIONARY_STRONG_REFERENCE, StrongReference(class(0x22))),
    t("IdentificationStrongReference", T_IDENTIFICATION_STRONG_REFERENCE, StrongReference(class(0x30))),
    t("TrackStrongReference", T_TRACK_STRONG_REFERENCE, StrongReference(class(0x38))),
    t("EssenceDescriptorStrongReference", T_ESSENCE_DESCRIPTOR_STRONG_REFERENCE, StrongReference(class(0x24))),
    t("ControlPointStrongReference", T_CONTROL_POINT_STRONG_REFERENCE, StrongReference(class(0x19))),
    t("DataDefinitionWeakReference", T_DATA_DEFINITION_WEAK_REFERENCE, WeakReference(class(0x1B), &[class(0x2F), u(0x060E2B34_01010102_06010104_02020000), u(0x060E2B34_01010102_06010104_05050000)])),
    t("OperationDefinitionWeakReference", T_OPERATION_DEFINITION_WEAK_REFERENCE, WeakReference(class(0x1C), &[class(0x2F), u(0x060E2B34_01010102_06010104_02020000), u(0x060E2B34_01010102_06010104_05030000)])),
    t("ParameterDefinitionWeakReference", T_PARAMETER_DEFINITION_WEAK_REFERENCE, WeakReference(class(0x1D), &[class(0x2F), u(0x060E2B34_01010102_06010104_02020000), u(0x060E2B34_01010102_06010104_05040000)])),
    t("ContainerDefinitionWeakReference", T_CONTAINER_DEFINITION_WEAK_REFERENCE, WeakReference(class(0x20), &[class(0x2F), u(0x060E2B34_01010102_06010104_02020000), u(0x060E2B34_01010102_06010104_05080000)])),
    t("InterpolationDefinitionWeakReference", T_INTERPOLATION_DEFINITION_WEAK_REFERENCE, WeakReference(class(0x21), &[class(0x2F), u(0x060E2B34_01010102_06010104_02020000), u(0x060E2B34_01010102_06010104_05090000)])),
    t("TaggedValueStrongReferenceVector", T_TAGGED_VALUE_STRONG_REFERENCE_VECTOR, VariableArray(T_TAGGED_VALUE_STRONG_REFERENCE)),
    t("SegmentStrongReferenceVector", T_SEGMENT_STRONG_REFERENCE_VECTOR, VariableArray(T_SEGMENT_STRONG_REFERENCE)),
    t("ParameterStrongReferenceVector", T_PARAMETER_STRONG_REFERENCE_VECTOR, VariableArray(T_PARAMETER_STRONG_REFERENCE)),
    t("ComponentStrongReferenceVector", T_COMPONENT_STRONG_REFERENCE_VECTOR, VariableArray(T_COMPONENT_STRONG_REFERENCE)),
    t("PackageStrongReferenceSet", T_PACKAGE_STRONG_REFERENCE_SET, Set(T_PACKAGE_STRONG_REFERENCE)),
    t("EssenceDataStrongReferenceSet", T_ESSENCE_DATA_STRONG_REFERENCE_SET, Set(T_ESSENCE_DATA_STRONG_REFERENCE)),
    t("ParameterDefinitionWeakReferenceSet", T_PARAMETER_DEFINITION_WEAK_REFERENCE_SET, Set(T_PARAMETER_DEFINITION_WEAK_REFERENCE)),
    t("OperationDefinitionStrongReferenceSet", T_OPERATION_DEFINITION_STRONG_REFERENCE_SET, Set(T_OPERATION_DEFINITION_STRONG_REFERENCE)),
    t("ParameterDefinitionStrongReferenceSet", T_PARAMETER_DEFINITION_STRONG_REFERENCE_SET, Set(T_PARAMETER_DEFINITION_STRONG_REFERENCE)),
    t("DataDefinitionStrongReferenceSet", T_DATA_DEFINITION_STRONG_REFERENCE_SET, Set(T_DATA_DEFINITION_STRONG_REFERENCE)),
    t("ContainerDefinitionStrongReferenceSet", T_CONTAINER_DEFINITION_STRONG_REFERENCE_SET, Set(T_CONTAINER_DEFINITION_STRONG_REFERENCE)),
    t("InterpolationDefinitionStrongReferenceSet", T_INTERPOLATION_DEFINITION_STRONG_REFERENCE_SET, Set(T_INTERPOLATION_DEFINITION_STRONG_REFERENCE)),
    t("LocatorStrongReferenceVector", T_LOCATOR_STRONG_REFERENCE_VECTOR, VariableArray(T_LOCATOR_STRONG_REFERENCE)),
    t("IdentificationStrongReferenceVector", T_IDENTIFICATION_STRONG_REFERENCE_VECTOR, VariableArray(T_IDENTIFICATION_STRONG_REFERENCE)),
    t("TrackStrongReferenceVector", T_TRACK_STRONG_REFERENCE_VECTOR, VariableArray(T_TRACK_STRONG_REFERENCE)),
    t("ControlPointStrongReferenceVector", T_CONTROL_POINT_STRONG_REFERENCE_VECTOR, VariableArray(T_CONTROL_POINT_STRONG_REFERENCE)),
];

/// The MetaDictionary object of a written file.
pub(crate) fn dictionary() -> Obj {
    let classes = CLASSES.iter().map(class_definition).collect();
    let types = TYPES.iter().map(type_definition).collect();
    Obj::new(META_DICTIONARY)
        .with(pid::META_CLASS_DEFINITIONS, Value::StrongSet(classes, pid::META_IDENTIFICATION))
        .with(pid::META_TYPE_DEFINITIONS, Value::StrongSet(types, pid::META_IDENTIFICATION))
}

fn definition(class: Auid, id: Auid, name: &str) -> Obj {
    Obj::new(class).data_prop(pid::META_IDENTIFICATION, id.to_vec()).data_prop(pid::META_NAME, utf16z(name))
}

fn boolean(b: bool) -> Vec<u8> {
    vec![u8::from(b)]
}

fn class_ref(id: Auid) -> Value {
    Value::Weak(path::META_CLASSES.to_vec(), pid::META_IDENTIFICATION, id.to_vec())
}

fn type_ref(id: Auid) -> Value {
    Value::Weak(path::META_TYPES.to_vec(), pid::META_IDENTIFICATION, id.to_vec())
}

/// A string array: the strings one after another, each NUL-terminated.
fn strings<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    names.into_iter().flat_map(utf16z).collect()
}

fn class_definition(c: &Class) -> Obj {
    let mut o =
        definition(meta::CLASS_DEFINITION, c.id, c.name).with(pid::CLASS_PARENT, class_ref(c.parent)).data_prop(pid::CLASS_IS_CONCRETE, boolean(c.concrete));
    if !c.props.is_empty() {
        o.set(pid::CLASS_PROPERTIES, Value::StrongSet(c.props.iter().map(property_definition).collect(), pid::META_IDENTIFICATION));
    }
    o
}

fn property_definition(p: &Prop) -> Obj {
    let mut o = definition(meta::PROPERTY_DEFINITION, p.id, p.name)
        .data_prop(pid::PROPERTY_TYPE, p.ty.to_vec())
        .data_prop(pid::PROPERTY_IS_OPTIONAL, boolean(p.usage == Opt))
        .data_prop(pid::PROPERTY_LOCAL_ID, p.pid.to_le_bytes().to_vec());
    if p.usage == Key {
        o.set(pid::PROPERTY_IS_UNIQUE, Value::Data(boolean(true)));
    }
    o
}

fn type_definition(t: &Type) -> Obj {
    match t.kind {
        Integer(size, signed) => {
            definition(meta::TYPE_INTEGER, t.id, t.name).data_prop(pid::INTEGER_SIZE, vec![size]).data_prop(pid::INTEGER_IS_SIGNED, boolean(signed))
        }
        Character => definition(meta::TYPE_CHARACTER, t.id, t.name),
        String(element) => definition(meta::TYPE_STRING, t.id, t.name).with(pid::STRING_ELEMENT_TYPE, type_ref(element)),
        Rename(renamed) => definition(meta::TYPE_RENAME, t.id, t.name).with(pid::RENAMED_TYPE, type_ref(renamed)),
        Record(members) => definition(meta::TYPE_RECORD, t.id, t.name)
            .with(pid::RECORD_MEMBER_TYPES, Value::WeakVec(path::META_TYPES.to_vec(), pid::META_IDENTIFICATION, members.iter().map(|m| m.1.to_vec()).collect()))
            .data_prop(pid::RECORD_MEMBER_NAMES, strings(members.iter().map(|m| m.0))),
        FixedArray(element, count) => definition(meta::TYPE_FIXED_ARRAY, t.id, t.name)
            .with(pid::FIXED_ARRAY_ELEMENT_TYPE, type_ref(element))
            .data_prop(pid::FIXED_ARRAY_COUNT, count.to_le_bytes().to_vec()),
        VariableArray(element) => definition(meta::TYPE_VARIABLE_ARRAY, t.id, t.name).with(pid::VARIABLE_ARRAY_ELEMENT_TYPE, type_ref(element)),
        Set(element) => definition(meta::TYPE_SET, t.id, t.name).with(pid::SET_ELEMENT_TYPE, type_ref(element)),
        StrongReference(class) => definition(meta::TYPE_STRONG_REFERENCE, t.id, t.name).with(pid::STRONG_REFERENCED_TYPE, class_ref(class)),
        WeakReference(class, targets) => definition(meta::TYPE_WEAK_REFERENCE, t.id, t.name)
            .with(pid::WEAK_REFERENCED_TYPE, class_ref(class))
            .data_prop(pid::WEAK_TARGET_SET, targets.concat()),
        Enumeration(element, values) => definition(meta::TYPE_ENUMERATION, t.id, t.name)
            .with(pid::ENUMERATION_ELEMENT_TYPE, type_ref(element))
            .data_prop(pid::ENUMERATION_NAMES, strings(values.iter().map(|v| v.0)))
            .data_prop(pid::ENUMERATION_VALUES, values.iter().flat_map(|v| v.1.to_le_bytes()).collect()),
        ExtendibleEnumeration(values) => definition(meta::TYPE_EXTENDIBLE_ENUMERATION, t.id, t.name)
            .data_prop(pid::EXTENDIBLE_ENUMERATION_NAMES, strings(values.iter().map(|v| v.0)))
            .data_prop(pid::EXTENDIBLE_ENUMERATION_VALUES, values.iter().map(|v| v.1).collect::<Vec<_>>().concat()),
        Indirect => definition(meta::TYPE_INDIRECT, t.id, t.name),
        Stream => definition(meta::TYPE_STREAM, t.id, t.name),
    }
}
