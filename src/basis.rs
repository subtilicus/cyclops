//! Basis Universal (ETC1S / UASTC) KTX2 textures.

use crate::gputex::TextureSet;

pub fn decode_ktx2(_bytes: &[u8], _first_only: bool) -> Result<TextureSet, String> {
    Err("Basis Universal (ETC1S/UASTC) KTX2 textures aren't supported yet".into())
}
