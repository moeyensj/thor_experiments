import pandas as pd
import argparse
import glob
import numpy as np

def read_log(log_file):
    with open(log_file, 'r') as f:
        lines = f.readlines()

    projection_time = np.nan
    projected_observations = np.nan
    clustering_time = np.nan
    restructuring_time = np.nan
    clusters = np.nan
    iod_time = np.nan
    iod_orbits = np.nan
    od_time = np.nan
    od_orbits = np.nan
    odp_time = np.nan
    odp_orbits = np.nan

    first_od = False
    for line in lines:
        contents = line.split("]")[2:]
        contents = "".join(contents).split(" ")[1:]
        if contents[0] == "Found" and contents[2] == "observations.":
            projected_observations = int(contents[1])
        elif contents[0] == "Range" and contents[2] == "shift":
            projection_time = float(contents[5])
        elif contents[0] == "Max" and contents[1] == "sample":
            eps = float(contents[3])
        elif contents[0] == "Found" and contents[2] == "clusters.":
            clusters = int(contents[1])
        elif contents[0] == "Clustering" and contents[1] == "completed":
            clustering_time = float(contents[3])
        elif contents[0] == "Restructuring" and contents[1] == "completed":
            restructuring_time = float(contents[3])
        elif contents[0] == "Found" and contents[2] == "initial":
            iod_orbits = int(contents[1])
        elif contents[0] == "Initial" and contents[1] == "orbit":
            iod_time = float(contents[5])
        elif contents[0] == "Differentially" and contents[1] == "corrected" and not first_od:
            od_orbits = int(contents[2])
        elif contents[0] == "Differential" and contents[1] == "correction" and not first_od:
            od_time = float(contents[4])
            first_od = True
        elif contents[0] == "Extended" and contents[2] == "merged":
            odp_orbits = int(contents[6])
        elif contents[0] == "Orbit" and contents[1] == "extension":
            odp_time = float(contents[6])



    data = {
        "eps": [eps * 3600],
        "projection_time": [projection_time],
        "projected_observations": [projected_observations],
        "clustering_time": [clustering_time],
        "restructuring_time": [restructuring_time],
        "clusters": [clusters],
        "iod_time": [iod_time],
        "iod_orbits": [iod_orbits],
        "od_time": [od_time],
        "od_orbits": [od_orbits],
        "odp_time": [odp_time],
        "odp_orbits": [odp_orbits]
    }
    df = pd.DataFrame(data)
    return df



if __name__ == "__main__":

    parser = argparse.ArgumentParser(description='Read log files')
    args = parser.parse_args()

    log_files = glob.glob("*/*/thor.log")
    dfs = []
    for log_file in log_files:

        contents = log_file.split("_")
        if contents[0] == "hotspot":
            alg = "hotspot_2d"
            cell_area = float(contents[3].split("/")[0][2:])
        elif contents[0] == "dbscan":
            alg = contents[0]
            cell_area = float(contents[2].split("/")[0][2:])

        df = read_log(log_file)
        df.insert(0, "cell_area", cell_area)
        df.insert(2, "alg", alg)
        dfs.append(df)

    df = pd.concat(dfs)
    df.sort_values(by=["cell_area", "eps", "alg"], inplace=True, ignore_index=True)

    df.to_csv("thor_results.csv", index=False)