// Read only the requested slice; the whole file is never base64 encoded.
export function readFilePart(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result;
      if (typeof result !== 'string' || !result.includes(',')) {
        reject(new Error('Não foi possível ler esta parte do arquivo.'));
        return;
      }
      resolve(result.slice(result.indexOf(',') + 1));
    };
    reader.onerror = () => reject(new Error('Não foi possível ler o arquivo selecionado.'));
    reader.onabort = () => reject(new Error('A leitura do arquivo foi cancelada.'));
    reader.readAsDataURL(file);
  });
}
