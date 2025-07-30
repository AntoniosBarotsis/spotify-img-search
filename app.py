import os
import torch
import numpy as np
from PIL import Image
from flask import Flask, request, render_template, jsonify
from transformers import CLIPProcessor, CLIPModel
from torch.utils.data import Dataset, DataLoader
import contextlib
import json
from tqdm import tqdm
import time

# Configuration
IMAGE_FOLDER = "./images/"
BATCH_SIZE = 64
MODEL_NAME = "openai/clip-vit-large-patch14"
EMBEDDINGS_FILE = "image_embeddings.pt"
METADATA_FILE = "embeddings_metadata.json"
DEVICE = "cuda" if torch.cuda.is_available() else "cpu"

app = Flask(__name__, static_folder=IMAGE_FOLDER)

# Global state to hold model and embeddings
app_state = {
    "embeddings": None,
    "metadata": None,
    "model": None,
    "processor": None,
    "autocast_context": None,
    "initialized": False,
}


# Custom dataset
class ImageDataset(Dataset):
    def __init__(self, image_folder):
        self.image_folder = image_folder
        self.image_files = sorted(
            [
                f
                for f in os.listdir(image_folder)
                if f.lower().endswith((".png", ".jpg", ".jpeg"))
            ]
        )

    def __len__(self):
        return len(self.image_files)

    def __getitem__(self, idx):
        img_path = os.path.join(self.image_folder, self.image_files[idx])
        try:
            image = Image.open(img_path).convert("RGB")
            return image, self.image_files[idx]
        except Exception as e:
            print(f"Error loading {self.image_files[idx]}: {str(e)}")
            return None, None


# Collate function
def collate_fn(batch):
    images = []
    filenames = []
    for image, filename in batch:
        if image is not None:
            images.append(image)
            filenames.append(filename)
    return images, filenames


# Initialize model
def initialize_model():
    model = CLIPModel.from_pretrained(MODEL_NAME, torch_dtype=torch.float32)
    processor = CLIPProcessor.from_pretrained(MODEL_NAME)
    model = model.to(DEVICE).eval()

    if DEVICE == "cuda":
        autocast_context = torch.amp.autocast(device_type="cuda", dtype=torch.float32)
    else:
        autocast_context = contextlib.nullcontext()

    return model, processor, autocast_context


# Precompute embeddings
def precompute_embeddings():
    model, processor, autocast_context = initialize_model()
    dataset = ImageDataset(IMAGE_FOLDER)
    dataloader = DataLoader(
        dataset,
        batch_size=BATCH_SIZE,
        collate_fn=collate_fn,
        pin_memory=True if DEVICE == "cuda" else False,
    )

    all_embeddings = []
    all_filenames = []

    progress_bar = tqdm(total=len(dataset), desc="Processing images", unit="img")
    total_processed = 0
    with torch.no_grad():
        for images, filenames in dataloader:
            if not images:  # Skip empty batches
                progress_bar.update(BATCH_SIZE)
                continue
            with autocast_context:
                inputs = processor(images=images, return_tensors="pt").to(DEVICE)
                image_features = model.get_image_features(**inputs)
                image_features = image_features / image_features.norm(
                    dim=-1, keepdim=True
                )

            all_embeddings.append(image_features.half().cpu())
            all_filenames.extend(filenames)
            batch_count = len(images)
            total_processed += batch_count
            progress_bar.update(batch_count)
            progress_bar.set_postfix(
                {"current": f"{batch_count} images", "total": total_processed}
            )

    all_embeddings = torch.cat(all_embeddings, dim=0)
    torch.save(all_embeddings, EMBEDDINGS_FILE)

    metadata = {
        "model": MODEL_NAME,
        "image_count": len(all_filenames),
        "embedding_dim": all_embeddings.shape[1],
        "filenames": all_filenames,
        "image_folder": os.path.abspath(IMAGE_FOLDER),
    }

    with open(METADATA_FILE, "w") as f:
        json.dump(metadata, f, indent=2)

    print(f"Precomputed {len(all_filenames)} embeddings")
    return all_embeddings, metadata, model, processor, autocast_context


# Load embeddings
def load_embeddings():
    if not os.path.exists(EMBEDDINGS_FILE) or not os.path.exists(METADATA_FILE):
        return precompute_embeddings()

    with open(METADATA_FILE, "r") as f:
        metadata = json.load(f)

    if os.path.abspath(IMAGE_FOLDER) != metadata["image_folder"]:
        return precompute_embeddings()

    current_files = set(os.listdir(IMAGE_FOLDER))
    saved_files = set(metadata["filenames"])

    if current_files != saved_files:
        return precompute_embeddings()

    embeddings = torch.load(EMBEDDINGS_FILE)
    model, processor, autocast_context = initialize_model()
    print(f"Loaded {len(metadata['filenames'])} embeddings")
    return embeddings, metadata, model, processor, autocast_context


# Initialize app state before first request
def initialize_app():
    if not app_state["initialized"]:
        print("Initializing model and embeddings...")
        results = load_embeddings()
        (
            app_state["embeddings"],
            app_state["metadata"],
            app_state["model"],
            app_state["processor"],
            app_state["autocast_context"],
        ) = results
        app_state["initialized"] = True
        print("Initialization complete")


# Search API endpoint
@app.route("/search", methods=["POST"])
def search():
    start_time = time.perf_counter()
    data = request.get_json()
    query = data["query"]
    top_k = int(data.get("top_k", 10))

    # Perform search
    results = []
    with torch.no_grad(), app_state["autocast_context"]:
        inputs_text = app_state["processor"](
            text=[query], return_tensors="pt", padding=True
        ).to(DEVICE)

        text_features = app_state["model"].get_text_features(**inputs_text)
        text_features = text_features / text_features.norm(dim=-1, keepdim=True)

        embeddings = app_state["embeddings"].to(DEVICE)
        text_features = text_features.to(embeddings.dtype)

        similarities = (embeddings @ text_features.T).squeeze().cpu().numpy()
        top_indices = np.argsort(similarities)[::-1][:top_k]

        for idx in top_indices:
            results.append(
                {
                    "filename": IMAGE_FOLDER + app_state["metadata"]["filenames"][idx],
                    "similarity": float(similarities[idx]),
                }
            )

    end_time = time.perf_counter()
    elapsed_ms = (end_time - start_time) * 1000

    return jsonify({"results":results, "elapsed_ms":elapsed_ms})


# Main page
@app.route("/")
def index():
    initialize_app()
    return render_template("index.html")


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000, debug=False)
